//! How long is a frame? (M6-11, #131)
//!
//! `cargo bench -p genesis --bench frame`, or `just bench`. It prints milliseconds
//! per [`genesis::Genesis::run_frame`] for two machines:
//!
//! * `synthetic` - a generated cartridge whose program paints the whole display:
//!   both scroll planes filled with 8x8 cells out of 32 patterns, and 80 sprites
//!   of 32x32 laid out so that every line of the picture carries ten of them and
//!   320 pixels of sprite - the per-line and per-line-pixel ceilings of
//!   genvdp.txt section 15 "Drawing Limitations" - and then sits in
//!   `addq.l #1,d0; bra.b -4`. That last part is deliberate: the 68000 then costs
//!   the scheduler one instruction per slice and nothing else, so the number is
//!   the cost of a full picture rather than of a busy CPU.
//! * `sonic1` - the same measurement on `Sonic The Hedgehog (USA, Europe).md`
//!   from `$RUSTSEGA_ROM_DIR`, booted for 600 frames with START on frames 300
//!   and 301 exactly as `tests/commercial.rs` runs it, so that its frame 600 is
//!   the picture `tests/golden/commercial.toml` holds a hash of. Skipped when
//!   that cartridge is not on disk, which is every machine but a developer's.
//!
//! # Why `Instant` and not `criterion`
//!
//! The ticket allows either. `Instant` it is, for three reasons. The number
//! wanted is one per-call wall time, best of several rounds, and criterion
//! reports a mean with a confidence interval - on a box where another build can
//! be compiling through the middle of a run, the mean is the wrong statistic and
//! the spread is the one that matters. Criterion is thirty-odd crates of
//! dev-dependency to compile for one number. And this bench has to print a skip
//! for an absent cartridge rather than measure nothing, which is easier in sixty
//! lines of `main` than inside a `criterion` benchmark.
//!
//! `cargo bench` builds this with `[profile.bench]`, which inherits the release
//! profile (`opt-level = 3`, `lto = "thin"`, `codegen-units = 1`), so these are
//! release numbers.
//!
//! # Reading it
//!
//! Each scenario is run `ROUNDS` times on a freshly built machine, `WARMUP`
//! frames of it discarded - the synthetic program's own scene is thousands of
//! data port writes and costs a frame or two - and `FRAMES` of it timed. The
//! headline is the best round, not the mean, and `spread` is worst minus best:
//! on a shared machine a "speedup" smaller than the spread is not a speedup.
//!
//! Every scenario prints two hashes as well. `digest` is FNV-1a 64 folded over
//! the [`testkit::frame_hash`] of every timed frame, so an optimisation that
//! changed a pixel shows up in the run that reports the timing. `frame` is the
//! hash of the last frame, which for `sonic1` is comparable with the committed
//! commercial golden.

use std::hint::black_box;
use std::time::{Duration, Instant};

use genesis::{Buttons, Config, Genesis};

/// Frames per timed round. Six hundred is ten seconds of picture: long enough
/// that a round runs to hundreds of milliseconds and the clock is not resolving
/// into noise, short enough that a round stays cheap.
const FRAMES: u32 = 600;

/// Warm-up frames, run and thrown away. The synthetic program paints its scene
/// with about five thousand VRAM words, and a VRAM word costs two access slots,
/// so the first frame or two of a machine are the scene being written rather
/// than the scene being drawn.
const WARMUP: u32 = 20;

/// Timed rounds per scenario. The best of them is the headline, so the more
/// rounds there are, the likelier one of them ran on an unloaded core.
const ROUNDS: usize = 7;

/// The 68000's VDP ports (plutiedev "Memory map").
const VDP_DATA: u32 = 0xC0_0000;
const VDP_CONTROL: u32 = 0xC0_0004;

/// Where the generated program sits: past the 0x200 bytes a header load needs
/// ([`genesis::cart::parse_header`]), and the address `tests/rom_harness.rs`
/// uses for its fake ROMs.
const CODE: u32 = 0x1000;
/// The reset stack pointer: the top of work RAM.
const STACK: u32 = 0x00FF_0000;

/// VRAM bases of the scene, as the registers below multiply out: plane A is
/// `(R2 & 0x38) << 10`, plane B `(R4 & 0x07) << 13`, and the sprite attribute
/// table `(R5 & 0x7E) << 9` in 40-cell mode (`crates/vdp/src/lib.rs`).
const PLANE_A: u16 = 0xC000;
const PLANE_B: u16 = 0xE000;
const SPRITE_TABLE: u16 = 0xF000;
/// Cells of one scroll plane: the 64x32 playfield R16 = 0x01 selects.
const PLANE_CELLS: usize = 64 * 32;
/// Patterns at the bottom of VRAM: 32 of them, 32 bytes (16 words) apiece
/// (genvdp.txt section 12: a pattern is an 8x8 block of 4 bits a pixel).
const PATTERN_COUNT: u16 = 32;
/// Sprites: the 80 of the 40-cell limit, 4x4 cells each, in a grid 10 across
/// and 8 down, so a visible line carries ten of them and 320 sprite pixels.
const SPRITE_COUNT: usize = 80;
const SPRITE_COLUMNS: usize = 10;
/// The side of one of those 4x4-cell sprites, in pixels.
const SPRITE_PX: u16 = 32;

fn main() {
    println!("rustsega frame benchmark (M6-11 #131)");
    check_scene();
    println!("{FRAMES} frames x {ROUNDS} rounds, {WARMUP} warm-up frames, release profile");
    println!();
    println!(
        "{:<10} {:>9} {:>9} {:>9} {:>9}  {:<19} {:<19}",
        "scenario", "best", "median", "worst", "spread", "digest", "frame"
    );

    report("synthetic", &|frames| {
        run_machine(synthetic_rom(Parts::ALL), frames, &[])
    });
    match sonic1() {
        Some(run) => report("sonic1", &run),
        None => println!(
            "{:<10} skipped: no cartridge in $RUSTSEGA_ROM_DIR",
            "sonic1"
        ),
    }
}

/// The share of the picture a layer has to change before a bench that says it is
/// measuring that layer is believed.
const MIN_LAYER_SHARE: f64 = 0.05;
/// The full scene has to show at least this many distinct pixels. It makes 46:
/// 15 colours of each of the three palettes the two scroll planes and the
/// sprites use, plus the backdrop. A scene that collapsed to one plane, or that
/// never had its patterns written, is well below this.
const MIN_COLOURS: usize = 30;

/// One measured run: the wall clock of `FRAMES` frames of one machine, and
/// enough about the last of them to tell a fast run from a run that drew nothing.
struct Scene {
    /// Wall clock of the timed frames.
    elapsed: Duration,
    /// Digest of every timed frame, oldest first.
    digest: u64,
    /// The hash of the last frame alone.
    frame_hash: u64,
    /// Distinct pixels of that last frame: a blank screen has one.
    colours: usize,
    /// That last frame itself, active display only. Kept for the scene checks,
    /// which compare pictures against each other rather than against hashes.
    pixels: Vec<u32>,
}

/// Time `make(FRAMES)` `ROUNDS` times and print the best / median / worst
/// milliseconds per frame, their spread, and the last round's hashes.
fn report(name: &str, make: &dyn Fn(u32) -> Scene) {
    let scenes: Vec<Scene> = (0..ROUNDS).map(|_| make(FRAMES)).collect();
    let last = scenes.last().expect("at least one round");

    let mut per_frame: Vec<f64> = scenes
        .iter()
        .map(|scene| scene.elapsed.as_secs_f64() * 1000.0 / f64::from(FRAMES))
        .collect();
    per_frame.sort_by(|a, b| a.total_cmp(b));

    println!(
        "{:<10} {:>8.3}ms {:>8.3}ms {:>8.3}ms {:>8.3}ms  {:#018x}  {:#018x}",
        name,
        per_frame[0],
        per_frame[ROUNDS / 2],
        per_frame[ROUNDS - 1],
        per_frame[ROUNDS - 1] - per_frame[0],
        last.digest,
        last.frame_hash,
    );
    println!("{:<10} {} colours in the last frame", "", last.colours);
}

/// The last picture of a machine, for the scene checks of [`check_scene`].
struct Still {
    /// The active display, row-major.
    pixels: Vec<u32>,
    /// [`testkit::frame_hash`] of it.
    hash: u64,
    /// Distinct values in `pixels`.
    colours: usize,
}

/// Which parts of the picture a generated cartridge paints. The benchmark wants
/// all of them; [`check_scene`] wants them one at a time, so that "the scene is
/// loaded" is an assertion about the pixels a layer changes rather than a claim
/// about a program the bench hopes it ran.
#[derive(Clone, Copy)]
struct Parts {
    plane_a: bool,
    plane_b: bool,
    sprites: bool,
}

impl Parts {
    const ALL: Parts = Parts {
        plane_a: true,
        plane_b: true,
        sprites: true,
    };
    const BACKDROP: Parts = Parts {
        plane_a: false,
        plane_b: false,
        sprites: false,
    };
    const NO_SPRITES: Parts = Parts {
        plane_a: true,
        plane_b: true,
        sprites: false,
    };
    const NO_PLANE_A: Parts = Parts {
        plane_a: false,
        plane_b: true,
        sprites: true,
    };
    const NO_PLANE_B: Parts = Parts {
        plane_a: true,
        plane_b: false,
        sprites: true,
    };

    /// The scene program of these parts, written to a cartridge and run to a
    /// still picture `frames` frames past the warm-up.
    fn still(self, frames: u32) -> Still {
        let machine = run_machine(synthetic_rom(self), frames, &[]);
        Still {
            pixels: machine.pixels,
            hash: machine.frame_hash,
            colours: machine.colours,
        }
    }
}

/// The scene's own test, and the reason its timings mean anything: every layer
/// the bench claims to measure has to change the picture it is measured on.
///
/// `docs/TESTING.md` is right that a golden of nothing is worse than no golden,
/// and the same goes for a benchmark. Without this the bench would happily report
/// a fast 1 ms of backdrop.
fn check_scene() {
    let all = Parts::ALL.still(10);
    let bare = Parts::BACKDROP.still(10);
    let no_sprites = Parts::NO_SPRITES.still(10);
    // `bare` writes no name table and no sprite list, and an empty name table is
    // all-zero entries pointing at pattern 0, which `tile_pixel` keeps empty for
    // exactly that reason: it is the picture of nothing being drawn.
    let no_plane_a = Parts::NO_PLANE_A.still(10);
    let no_plane_b = Parts::NO_PLANE_B.still(10);

    // The warm-up was long enough: a picture that is still moving between frame
    // `WARMUP + 10` and `WARMUP + 40` is the program still writing its scene, and
    // every number below would be a number about a half-built machine.
    let later = Parts::ALL.still(40);
    assert_eq!(
        all.hash,
        later.hash,
        "the synthetic scene is not settled by frame {}",
        WARMUP + 10
    );

    assert_eq!(
        bare.colours, 1,
        "with no planes and no sprites the display is the backdrop, so a scene with \
         nothing in it cannot be mistaken for one that draws"
    );
    assert!(
        all.colours >= MIN_COLOURS,
        "the full scene shows {} colours, below the {} the three palettes of a loaded \
         plane pair are expected to make",
        all.colours,
        MIN_COLOURS,
    );
    assert!(
        all.hash != no_sprites.hash && differs(&all.pixels, &no_sprites.pixels) > MIN_LAYER_SHARE,
        "the sprite layer is not being drawn: it changes {:.1}% of the picture",
        100.0 * differs(&all.pixels, &no_sprites.pixels),
    );
    assert!(
        all.hash != no_plane_a.hash && differs(&all.pixels, &no_plane_a.pixels) > MIN_LAYER_SHARE,
        "plane A is not being drawn: it changes {:.1}% of the picture",
        100.0 * differs(&all.pixels, &no_plane_a.pixels),
    );
    assert!(
        all.hash != no_plane_b.hash && differs(&all.pixels, &no_plane_b.pixels) > MIN_LAYER_SHARE,
        "plane B is not being drawn: it changes {:.1}% of the picture",
        100.0 * differs(&all.pixels, &no_plane_b.pixels),
    );
    println!(
        "scene ok: {} colours, sprites move {:.0}% of it, plane A {:.0}%, plane B {:.0}%",
        all.colours,
        100.0 * differs(&all.pixels, &no_sprites.pixels),
        100.0 * differs(&all.pixels, &no_plane_a.pixels),
        100.0 * differs(&all.pixels, &no_plane_b.pixels),
    );
}

/// The share of a frame two pictures disagree about.
fn differs(one: &[u32], other: &[u32]) -> f64 {
    let same = one.iter().zip(other.iter()).filter(|(a, b)| a == b).count();
    1.0 - (same as f64) / (one.len().max(1) as f64)
}

/// Run `rom` for `WARMUP + frames` frames, timing only `run_frame` on the last
/// `frames` of them, and fold a digest of every timed picture in on the way past.
///
/// `inputs` are the frames on which port 1 holds a button, in the same 1-based
/// convention `tests/common` uses, so a run of a real cartridge here is the run
/// its golden was made from.
fn run_machine(rom: Vec<u8>, frames: u32, inputs: &[(u32, Buttons)]) -> Scene {
    let mut machine =
        Genesis::new(&rom, Config::default()).expect("the benchmark image loads as a cartridge");
    let mut digest = FNV_OFFSET;
    // The clock is read around `run_frame` and nowhere else: the digest below is
    // a hash of 286 KB of pixels a frame, which is a tenth of a frame's cost and
    // is not what is being measured.
    let mut elapsed = Duration::ZERO;
    for frame in 1..=(frames + WARMUP) {
        let held = inputs
            .iter()
            .find(|(number, _)| *number == frame)
            .map_or_else(Buttons::empty, |(_, buttons)| *buttons);
        machine.set_pad(0, held);
        let start = Instant::now();
        machine.run_frame();
        if frame > WARMUP {
            elapsed += start.elapsed();
        }
        if frame > WARMUP {
            digest = frame_digest(digest, &machine);
        }
    }
    black_box(&machine);

    let frame = machine.frame();
    let pixels = frame.pixels[..frame.width * frame.height].to_vec();
    let frame_hash = testkit::frame_hash(&pixels, frame.width, frame.height);
    Scene {
        elapsed,
        digest,
        frame_hash,
        colours: distinct_colours(&pixels),
        pixels,
    }
}

/// `Sonic The Hedgehog (USA, Europe)`, run the way `tests/commercial.rs` runs
/// it, or `None` when the cartridge is not on this machine.
fn sonic1() -> Option<impl Fn(u32) -> Scene> {
    let path = testkit::commercial_rom("Sonic The Hedgehog (USA, Europe).md")?;
    let bytes = std::fs::read(&path).expect("read the Sonic 1 cartridge");
    println!("\nsonic1 cartridge: {}", path.display());
    Some(move |frames: u32| {
        run_machine(
            bytes.clone(),
            frames,
            &[(300, Buttons::START), (301, Buttons::START)],
        )
    })
}

// ---------------------------------------------------------------------------
// The scene digest
// ---------------------------------------------------------------------------

/// FNV-1a 64 basis, as `testkit::frame_hash` uses.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64 prime.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Fold one more frame into the digest, by running its hash through the same
/// FNV-1a update. Any change to any frame of the run, and any change to the
/// order of two frames, moves the digest.
fn frame_digest(digest: u64, machine: &Genesis) -> u64 {
    let frame = machine.frame();
    let pixels = &frame.pixels[..frame.width * frame.height];
    let hash = testkit::frame_hash(pixels, frame.width, frame.height);
    let mut digest = digest;
    for byte in hash.to_le_bytes() {
        digest ^= u64::from(byte);
        digest = digest.wrapping_mul(FNV_PRIME);
    }
    digest
}

/// Distinct pixel values of a frame. A sort rather than a set because a frame is
/// 71,680 pixels and a `HashSet` of them is a lot of hashing for an answer that
/// is only ever printed.
fn distinct_colours(pixels: &[u32]) -> usize {
    let mut sorted = pixels.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    sorted.len()
}

// ---------------------------------------------------------------------------
// The synthetic cartridge
// ---------------------------------------------------------------------------

/// A cartridge image that paints [`parts`]' picture and then busy-loops for
/// ever: the reset vectors for [`STACK`] / [`CODE`], and the program at [`CODE`].
fn synthetic_rom(parts: Parts) -> Vec<u8> {
    let program = scene_program(parts);
    let mut rom = vec![0_u8; CODE as usize + 2 * program.len()];
    rom[0..4].copy_from_slice(&STACK.to_be_bytes());
    rom[4..8].copy_from_slice(&CODE.to_be_bytes());
    for (index, word) in program.iter().enumerate() {
        let at = CODE as usize + 2 * index;
        rom[at..at + 2].copy_from_slice(&word.to_be_bytes());
    }
    rom
}

/// The program: bring the VDP up, write the scene through the data port, turn
/// the display on, and loop.
///
/// Every write is a `move.w #imm, (addr).L`, `33FC imm16 addr32` - the encoding
/// `tests/rom_harness.rs` uses - because that is the only instruction this
/// generator has to get right. It costs eight bytes a write, which is why the
/// image is tens of kilobytes of instructions, and why it is generated.
fn scene_program(parts: Parts) -> Vec<u16> {
    let mut program = Vec::with_capacity(80_000);

    // Display off first: the scene goes in over the frames it takes to write,
    // and a half-painted picture on the way up would make those frames differ
    // from every frame after them.
    vdp_register(&mut program, 1, 0x00);
    // R15 = 2: the address register steps on by a word after every data port
    // access (genvdp.txt section 7), which is what lets a whole table be
    // written from one command word.
    vdp_register(&mut program, 15, 0x02);
    vdp_register(&mut program, 0, 0x00);
    vdp_register(&mut program, 12, 0x01);
    vdp_register(&mut program, 2, 0x30);
    vdp_register(&mut program, 4, 0x07);
    vdp_register(&mut program, 5, 0x78);
    vdp_register(&mut program, 3, 0x00);
    vdp_register(&mut program, 13, 0x3F);
    vdp_register(&mut program, 11, 0x03);
    vdp_register(&mut program, 16, 0x01);
    vdp_register(&mut program, 7, 0x00);

    cram(&mut program);
    patterns(&mut program);
    if parts.plane_a {
        name_table(&mut program, PLANE_A, plane_a_entry);
    }
    if parts.plane_b {
        name_table(&mut program, PLANE_B, plane_b_entry);
    }
    if parts.sprites {
        sprite_table(&mut program);
    }

    // Display on, then the loop the frame cost is measured around:
    // `addq.l #1,d0` and `bra.b` back over it, the pair `Scheduler`'s own tests
    // bill at 8 + 10 clocks.
    vdp_register(&mut program, 1, 0x40);
    program.push(0x5280);
    program.push(0x60FC);
    program
}

/// The 64 CRAM words, in colours that move with the index so that a pixel landing
/// in the wrong palette, or the wrong colour of a line, is a different pixel.
///
/// `----bbb0 ggg0 rrr0` (genvdp.txt section 9).
fn cram(program: &mut Vec<u16>) {
    vdp_command(program, 0xC000, 0x0000);
    for index in 0..64_u16 {
        let red = index & 7;
        let green = (index >> 3) & 7;
        let blue = ((index >> 1) ^ (index * 5)) & 7;
        data(program, (blue << 10) | (green << 5) | red);
    }
}

/// 32 patterns at the bottom of VRAM: 8 rows of 2 words apiece.
///
/// Pattern 0 is all colour 0 - "A pixel within a pattern that uses value zero
/// isn't shown" (genvdp.txt section 12) - so an empty name table draws nothing,
/// which is what lets [`check_scene`] see the bare backdrop. Of the rest the
/// first half are opaque, so a plane drawn with them covers what is behind it,
/// and the second half are holes on a checkerboard, so plane A lets plane B
/// through in places and the scene has depth in it.
fn patterns(program: &mut Vec<u16>) {
    vram_write_at(program, 0);
    for tile in 0..PATTERN_COUNT {
        for row in 0..8_u16 {
            for half in 0..2_u16 {
                let base = half * 4;
                let word = (0..4).fold(0_u16, |word, offset| {
                    let pixel = tile_pixel(tile, row, base + offset);
                    word | (pixel << (12 - 4 * offset))
                });
                data(program, word);
            }
        }
    }
}

/// The 4-bit colour of pixel `column` of `row` of a pattern: nibbles go
/// high-first, since "the high nibble of a byte is the left pixel"
/// ([`crate::pattern`], as `crates/vdp/src/pattern.rs`'s `tile_row` documents).
fn tile_pixel(tile: u16, row: u16, column: u16) -> u16 {
    if tile == 0 {
        return 0;
    }
    let holed = tile > PATTERN_COUNT / 2 && (row + column).is_multiple_of(2);
    let colour = (tile * 5 + row * 3 + column * 7) % 15;
    if holed { 0 } else { colour + 1 }
}

/// Fill the name table at `base` with `entry` of each of its [`PLANE_CELLS`]
/// cells.
fn name_table(program: &mut Vec<u16>, base: u16, entry: fn(usize) -> u16) {
    vram_write_at(program, base);
    for cell in 0..PLANE_CELLS {
        data(program, entry(cell));
    }
}

/// Plane A's cell `cell`: palette 2, a tile that walks the whole pattern set,
/// the two flips varied so that the fetch is not the same fetch ten times
/// across a row, and the priority bit of genvdp.txt section 14 on every eighth
/// column.
fn plane_a_entry(cell: usize) -> u16 {
    let column = (cell % 64) as u16;
    let row = (cell / 64) as u16;
    let tile = (cell as u16) % PATTERN_COUNT;
    let priority = u16::from(column % 8 == 7) << 15;
    let vflip = u16::from(row % 2 == 1) << 12;
    let hflip = u16::from(column % 2 == 1) << 11;
    priority | (2 << 13) | vflip | hflip | tile
}

/// Plane B's cell `cell`: palette 1, and tiles slow enough across a row that
/// plane A's holes let something recognisable through.
fn plane_b_entry(cell: usize) -> u16 {
    (1 << 13) | ((cell / 2) as u16 % PATTERN_COUNT)
}

/// The sprite attribute table: [`SPRITE_COUNT`] sprites of 4x4 cells, in a grid
/// [`SPRITE_COLUMNS`] across, so a visible line of the picture carries ten of
/// them and 320 pixels of sprite (genvdp.txt section 15).
///
/// The table is 8-byte entries of y, size-and-link, pattern, x (section 15), and
/// coordinates are in the virtual 512x512 space that section gives, whose
/// display starts at 128,128.
fn sprite_table(program: &mut Vec<u16>) {
    vram_write_at(program, SPRITE_TABLE);
    for index in 0..SPRITE_COUNT {
        let index16 = index as u16;
        let column = (index % SPRITE_COLUMNS) as u16;
        let row = (index / SPRITE_COLUMNS) as u16;
        let y = 128 + row * SPRITE_PX;
        let x = 128 + column * SPRITE_PX;
        let link = if index + 1 == SPRITE_COUNT {
            0
        } else {
            index as u16 + 1
        };
        let size = (3 << 10) | (3 << 8) | link;
        // Word 2 is `pccvhnnn nnnnnnnn`; sprites have the last three palettes,
        // and every third one gets the priority bit so the compositor of
        // section 14 sees both of its sprite layers.
        let priority = u16::from(index % 3 == 0) << 15;
        let pattern = priority | (3 << 13) | (index16 % PATTERN_COUNT);
        data(program, y);
        data(program, size);
        data(program, pattern);
        data(program, x);
    }
}

/// `move.w #imm, (addr).L`: `33FC imm16 addr32`.
fn store_word(program: &mut Vec<u16>, addr: u32, value: u16) {
    let addr = addr.to_be_bytes();
    program.extend_from_slice(&[
        0x33FC,
        value,
        u16::from_be_bytes([addr[0], addr[1]]),
        u16::from_be_bytes([addr[2], addr[3]]),
    ]);
}

/// One VDP register write: `100r rrrr dddd dddd` to the control port
/// (genvdp.txt section 7 "Programming VDP registers").
fn vdp_register(program: &mut Vec<u16>, reg: u8, value: u8) {
    let word = 0x8000 | (u16::from(reg) << 8) | u16::from(value);
    store_word(program, VDP_CONTROL, word);
}

/// Both halves of a data port command word, in order.
fn vdp_command(program: &mut Vec<u16>, first: u16, second: u16) {
    store_word(program, VDP_CONTROL, first);
    store_word(program, VDP_CONTROL, second);
}

/// Point the data port at VRAM address `addr` for a write: CD1-CD0 = `01` and
/// A13-A0 in the first half, CD5-CD2 and A15-A14 in the second (genvdp.txt
/// section 7, and `Bus`'s own `vram_write_cmd` of `crates/genesis/src/bus.rs`).
fn vram_write_at(program: &mut Vec<u16>, addr: u16) {
    vdp_command(program, 0x4000 | (addr & 0x3FFF), (addr >> 14) & 0x0003);
}

/// One word out of the data port.
fn data(program: &mut Vec<u16>, word: u16) {
    store_word(program, VDP_DATA, word);
}
