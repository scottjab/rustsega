//! Save-state round trip of the VDP state (M6-07, #127).
//!
//! Drive a chip into a mid-frame state through the ports and the beam, serialise
//! it with postcard, read it back, then advance both copies and require the same
//! picture and the same state at the end of it.
//!
//! The continued run is the real assertion. A chip restored with its beam back on
//! line 0, its write FIFO empty, its line latch missing or its sprite list
//! unwalked would still serialise and still draw *a* picture — but not the
//! picture the chip that was saved was halfway to, and running the two copies on
//! is what catches that. `Vdp` has no `PartialEq`, so the state itself is
//! compared the way `Ym2612`'s is (#126): by the postcard bytes of the two
//! copies, which agree only if every field of the chip does.

use vdp::{DmaBus, Vdp};

/// Master clocks in one raster line: `hv::MCLKS_PER_LINE`, which [`Vdp::advance`]
/// counts in (genvdp.txt section 5 "Timing").
const MCLKS_PER_LINE: u32 = 3_420;

/// Status bit 1 (DMA busy), genvdp.txt section 6.
const BIT_DMA_BUSY: u16 = 1 << 1;

/// Lines of one NTSC raster (genvdp.txt section 5 "Timing"), for advancing a
/// whole frame at a time.
const NTSC_LINES: u32 = 262;

/// Control port words that set up a VRAM write at `addr` (genvdp.txt section 7
/// "VDP ports": code `4000`, the top two address bits riding in the second half).
fn vram_write_cmd(addr: u16) -> (u16, u16) {
    (0x4000 | (addr & 0x3FFF), (addr >> 14) & 0x0003)
}

/// Is a DMA running right now? Read straight out of the status register, which
/// reports bit 1 without clearing it (`status::Vdp::read_status`).
fn dma_busy(vdp: &mut Vdp) -> bool {
    vdp.read_status() & BIT_DMA_BUSY != 0
}

/// Write `value` to VRAM word `addr` through the data port, then run a line so
/// the access slots the write waits for come round: `ports` queues a write rather
/// than performing it (M4-21a, #99).
fn put_vram(vdp: &mut Vdp, addr: u16, value: u16) {
    let (first, second) = vram_write_cmd(addr);
    vdp.write_control(first);
    vdp.write_control(second);
    vdp.write_data(value);
    vdp.advance(MCLKS_PER_LINE);
}

/// The 68k memory a 68k -> VDP DMA reads from: the word at byte address `addr`.
struct DmaSource(&'static [u16]);

impl DmaBus for DmaSource {
    fn read_word(&mut self, addr: u32) -> u16 {
        self.0[(addr as usize / 2) % self.0.len()]
    }
}

/// Set up a 68k -> VDP transfer of `words` words from byte address `src`, in the
/// order a program sets one up: R19-R23, R1 M1 to allow it (plutiedev "$81xx"),
/// then the command word with CD5 that asks for it (genvdp.txt section 11).
fn dma_from(vdp: &mut Vdp, src: u32, words: u16) {
    vdp.write_register(19, words as u8);
    vdp.write_register(20, (words >> 8) as u8);
    vdp.write_register(21, (src >> 1) as u8);
    vdp.write_register(22, (src >> 9) as u8);
    vdp.write_register(23, ((src >> 17) as u8) & 0x7F);
    vdp.write_register(1, vdp.reg(1) | 0x10); // R1 M1: DMA allowed
    vdp.write_control(vram_write_cmd(0x0000).0);
    vdp.write_control(vram_write_cmd(0x0000).1 | 0x0080);
    assert!(vdp.dma_requested(), "a command word with CD5 asks for it");
}

/// A chip that has been running: registers written through the port, colours in
/// CRAM, a pattern row and a name table entry in VRAM, and a line and a third of
/// beam. A DMA in flight is the other interesting mid-state, and
/// [`vdp_state_roundtrip_mid_dma`] is the one that saves inside one.
///
/// The register values are the ones `compose`'s tests use, in the order a boot
/// program writes them (plutiedev "VDP registers"). R1 `0x50` is the display on
/// with the line interrupt enabled, R10 `37` is a reload that expires inside the
/// run below, and R15 `1` is the auto-increment the VRAM writes rely on.
fn running() -> Vdp {
    let mut vdp = Vdp::new(false);
    for (reg, value) in [
        (0u8, 0x00u8),
        (1, 0x50),
        (2, 0x11),
        (3, 0x22),
        (4, 0x04),
        (5, 0x0C),
        (6, 0xB2),
        (7, 0x05),
        (10, 37),
        (12, 0x00),
        (15, 1),
    ] {
        vdp.write_register(reg, value);
    }
    // Colours: R7 above selects backdrop index 5, and these are the entries a
    // drawn line has to pick between - index 0, the backdrop, and one more each
    // for the two palette lines the pattern row below walks.
    vdp.cram[0] = 0x0EEE;
    vdp.cram[5] = 0x000E;
    vdp.cram[16] = 0x0E00;

    // One pattern row and one plane A name table entry pointing at it, so the
    // line the beam draws has something to draw.
    put_vram(&mut vdp, 0x0000, 0x77AA);
    put_vram(&mut vdp, 0x0E00, 0x0081);

    // One line and a third on top of the two the VRAM writes cost: the frame
    // lines before this one are drawn and gone, and the beam sits inside a line
    // part way through the frame with that line's latch taken and its sprite
    // list searched.
    vdp.advance(MCLKS_PER_LINE + MCLKS_PER_LINE / 3);
    vdp
}

/// The frame of `vdp` as one flat list, for comparing two copies.
fn picture(vdp: &Vdp) -> Vec<u32> {
    let frame = vdp.frame();
    frame.pixels[..frame.width * frame.height].to_vec()
}

/// The outside view of the chip that a test can compare without [`Vdp`]'s
/// `Debug` printing 64 KiB of VRAM: the status word, the registers and the beam.
///
/// `read_status` clears the two sprite flags it reports (genvdp.txt section 6),
/// so this is called once per chip and what it returns is what is compared.
fn view(vdp: &mut Vdp) -> (u16, Vec<u8>, u16) {
    let status = vdp.read_status();
    let regs = (0..24_u8).map(|reg| vdp.reg(reg)).collect();
    (status, regs, vdp.hv_counter())
}

#[test]
fn vdp_state_roundtrip() {
    let mut vdp = running();

    assert_ne!(
        picture(&vdp),
        vec![0; vdp.frame().width * vdp.frame().height],
        "the run above has to leave a drawn frame or the round trip proves nothing"
    );
    let counter = vdp.hv_counter();
    assert!(
        counter >> 8 >= 1 && counter & 0xFF != 0,
        "and the beam is inside a line part way into the frame, which is the \
         mid-frame position the state has to carry (counter {counter:#06x})"
    );

    let bytes = postcard::to_allocvec(&vdp).expect("serialise Vdp");
    let mut restored: Vdp = postcard::from_bytes(&bytes).expect("deserialise Vdp");

    // Every field of the chip, by the only equality `Vdp` offers.
    let again = postcard::to_allocvec(&restored).expect("serialise the restored Vdp");
    assert_eq!(
        again, bytes,
        "a restored VDP does not hold the state the saved one had"
    );
    assert_eq!(
        restored.vram, vdp.vram,
        "and the memories are there under the encoding: the one place a \
         accidentally-skipped field would go unnoticed"
    );
    let geometry = |vdp: &Vdp| (vdp.frame().width, vdp.frame().height);
    assert_eq!(
        geometry(&restored),
        geometry(&vdp),
        "the size of the picture is in a save state even though its pixels are not"
    );
    assert!(
        picture(&restored).iter().all(|&pixel| 0 == pixel),
        "the picture itself is not: the frame buffer is what the next frame draws, \
         and 600 KiB of it does not go to a browser's storage \
         (docs/save-state-format.md, `frame_state`)"
    );

    // And it is runnable from there: a third of a line at a time, with the same
    // data port traffic, and the two copies have to agree on every line the
    // restored one draws. Only the rows from the saved beam position down are
    // comparable, because the rows above it are the picture the state does not
    // carry and that this chip has not drawn yet.
    let drawn = vdp.hv_counter() >> 8;
    for step in 0..9 {
        vdp.write_data(0x0F0F);
        restored.write_data(0x0F0F);
        vdp.advance(MCLKS_PER_LINE / 3);
        restored.advance(MCLKS_PER_LINE / 3);

        let (a, b) = (vdp.frame(), restored.frame());
        assert_eq!(geometry(&vdp), geometry(&restored), "step {step}");
        let from = usize::from(drawn).min(a.height) * a.width;
        assert_eq!(
            &b.pixels[from..a.width * a.height],
            &a.pixels[from..a.width * a.height],
            "step {step}: a line the saved chip drew is not the line the restored \
             one drew, so some part of the beam, the latch, the sprite list or the \
             memories did not survive the trip"
        );
    }

    // Then whole rasters, where every row has been drawn and the comparison is
    // of the whole picture.
    for _ in 0..3 {
        vdp.write_data(0x0F0F);
        restored.write_data(0x0F0F);
        vdp.advance(MCLKS_PER_LINE * NTSC_LINES);
        restored.advance(MCLKS_PER_LINE * NTSC_LINES);
        assert_eq!(
            picture(&restored),
            picture(&vdp),
            "a restored VDP drew a different raster from the chip it was saved from"
        );
    }
    assert_eq!(
        view(&mut restored),
        view(&mut vdp),
        "status, registers and beam disagree after the trip"
    );
}

/// A chip saved with a 68k -> VDP DMA in flight finishes that DMA after the
/// restore, rather than losing the words it had already read.
#[test]
fn vdp_state_roundtrip_mid_dma() {
    let mut vdp = Vdp::new(false);
    vdp.write_register(15, 1);
    dma_from(&mut vdp, 0x000100, 64);
    let mut source = DmaSource(&[0xA55A; 8]);
    vdp.run_dma(&mut source);
    vdp.advance(100);
    assert!(
        dma_busy(&mut vdp),
        "the transfer has to still be running to save inside one"
    );

    let bytes = postcard::to_allocvec(&vdp).expect("serialise Vdp mid-DMA");
    let mut restored: Vdp = postcard::from_bytes(&bytes).expect("deserialise Vdp mid-DMA");

    // Run the rest of the transfer out on both and require the same VRAM.
    for _ in 0..20 {
        vdp.advance(MCLKS_PER_LINE);
        restored.advance(MCLKS_PER_LINE);
    }
    assert!(
        !dma_busy(&mut vdp),
        "the transfer has to have finished or the comparison below is empty"
    );
    assert_eq!(
        restored.vram, vdp.vram,
        "the words still in flight did not land where they were going"
    );
}
