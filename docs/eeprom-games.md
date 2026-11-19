# EEPROM saves on the cartridge bus

Some Mega Drive carts keep their save in a serial EEPROM hanging off two bits of
the cartridge data bus rather than in parallel battery-backed RAM at
`0xA151FF` ([`sram-games.md`](sram-games.md) is the list for that shape). The
device is `crates/genesis/src/cart/eeprom.rs`; this is where each game's chip and
board come from.

A cart says it has one with extra-memory type `$E8` at `0x1B2`:

```text
0x1B0  'R' 'A'            - the extra-memory record, if there is one
0x1B2   $E8               - the save is on the bus, not in the map
0x1B3   $20 | $00         - usually the $20 of 0x200000
0x1B4   0x200000          - start of the window the lines live on
0x1B8   0x200001          - end of it, often the same address again
```

`$E8` is not save RAM and nothing else: `cart::sram` refuses it, and this is the
device it leaves behind (`sram::tests::eeprom_is_left_to_m3_13`). A cart has one
save shape or the other. The window is a pair of addresses, because it names the
two halves of one 16-bit cart access rather than a length - `0x200001..0x200001`
means "the clock and the data are D0 and D1 of the byte at $200001", and the
emulator takes it as the pair $200000/$200001. `single_address_window_is_a_pair`
pins that, and `eeprom.rs`'s `EepromInfo::declared` is the one place it is
decided.

## The boards

Which bits of the data bus carry the two lines is the board's business, not the
chip's, and there are four of them here (`eeprom::Board`). They are the bit
positions the ticket specifies, and Genesis Plus GX's `eeprom_i2c.c` agrees line
for line in the `mapper_i2c_*_init` functions at the end of that file, which set
`scl_in_bit`, `sda_in_bit` and `sda_out_bit` to exactly these numbers.

| Board | clock | data in | data out | writes |
| --- | --- | --- | --- | --- |
| `Sega` | D1 | D0 | D0 | the odd byte only |
| `Ea` | D6 | D7 | D7 | the odd byte only |
| `Acclaim16M` | D1 | D0 | **D1** | either byte |
| `Acclaim32M` | D0 | D0 | D0 | **even byte for the clock, odd for the data** |
| `Codemasters` (J-CART, #356) | D1 | D0 | **D7** | either byte, over the whole quadrant `$300000-$37FFFF` |

Genesis Plus GX routes byte writes to the odd byte on the Sega and EA boards and
to either byte on the 16 Mbit board, which is the `Lane` field of `Pin`; the 32
Mbit board splits them, `mapper_acclaim_32M_write8` taking `/SCL` from the even
byte and `/SDA` from the odd one; and the Codemasters board is its
`mapper_i2c_jcart_init`, which installs the line writes over `$300000-$37ffff`
with the comment "/LWR & /UWR are unused" and the answer over the next
quadrant. The Codemasters board is the one whose lines are not a pair of
addresses: the whole `$300000-$37FFFF` quadrant drives them and the answer
comes back from `$380000-$3FFFFF`, where the J-CART register lives for the
carts that have both - the register's odd byte is `D7`-for-`jcart_read`'s own
bit 7 masked away *because* the line is wired there (`mapper_i2c_jcart_read8`'s
`(eeprom_i2c_out() << 7) | (jcart_read(address) & 0x7f)`), which is the
ownership rule `crates/genesis/src/cart/jcart.rs` states in code.

**Word accesses are two byte accesses.** `Bus::read_rom_word` and
`Bus::write_rom_word` go through `read_rom_byte`/`write_rom_byte` twice, by the
bus's own rule for the cartridge area (`Bus::read_rom_word`'s comment). Every
board above was checked against that rather than assumed, and all four survive
it: no mapping needs the two halves of a 16-bit access to arrive together,
because each half carries one line's level on its own and the state after both
is the state the board reaches. What a *word write* does on the way there is
worth knowing about on one board, and is the next section. The tests are
`bus::tests::eeprom_word_accesses_are_two_byte_accesses` (the 16 Mbit board) and
`bus::tests::acclaim_32m_board_splits_the_two_lines_over_the_bus`.

Two differences from hardware follow from it and are worth knowing:

- On the Acclaim 16 Mbit board a *word* write drives the lines from the low byte
  of the word, as the board does, but our decomposition has already driven them
  once from the high byte. A game that wrote the same byte to both halves, which
  is the natural way to write a byte-wide register through a 16-bit latch, gets
  the hardware's answer; a game that wrote different bits to the two halves
  would see one transition more than the real board makes. No cart is known to
  do that, and byte writes - what the declared `$E8` window describes - are
  exact.
- The 32 Mbit board's word write is not a line write at all on hardware:
  it is the board's read-window latch, which this emulator now does (see "The
  32 Mbit board's read-window latch" below). Byte writes drive the lines as
  they should.

## The 32 Mbit board's read-window latch

A word write to $200000-$2FFFFF on one of the Acclaim 32 Mbit carts does not
move the lines. Both /LWR and /UWR asserted is the board's PAL - Genesis Plus
GX's "custom LZ95A53", the custom mapper chip of the 670125 and 670127 boards -
latching bit 0 of the written word into a one-bit read mux (GPGX's
[`mapper_acclaim_32M_write16`](https://github.com/ekeeke/Genesis-Plus-GX/blob/master/core/cart_hw/eeprom_i2c.c),
whose comment calls it "custom bankshifting"; no bank number is latched and ROM
is never swapped for ROM). Bit 0 = 1 restores plain ROM reads over the whole
quadrant, and that is the reset state - the PAL's only address inputs are A20
and A21, so its window is the 1 MiB quadrant and nothing smaller, and /RES is
one of its inputs (8BitWizard's teardown of the board,
[spritesmind t=107](http://gendev.spritesmind.net/forum/viewtopic.php?t=107)).
Bit 0 = 0 arms the chip: every byte the quadrant reads answers with the /SDA
line level in bit 0 (GPGX's quadrant-wide `mapper_i2c_generic_read8`), which is
how a game samples the answer without the ROM's output driving the line under
it.

The games' own wrappers agree, and they are where the shape is pinned: every
EEPROM operation on these carts runs arm, bit-bang, restore. NBA Jam TE's
wrapper is `move.w #0,(A0)` at $000ADA to arm, the byte-write bit-banger
sampling `move.b (A1),Dn; andi.b #1,Dn`, then `move.w #1,(A0)` at $000AB2 to
restore; NFL Quarterback Club has the same pair at $0142FA and $01428C, and
QBC 96, Big Hurt and College Slam share the shape. In this emulator the latch
is [`Eeprom::write_word`] on the same board as the lines, and
[`Bus::write_rom_word`] hands the word there first because the two byte writes
a word write usually decomposes into would drive lines the board does not
drive.

One correction this latch makes to what was here before: the pair at $200000-
$200001 used to answer with the line in bit 0 whatever the board's window said,
which corrupted bit 0 of every ROM read of $200001 while no operation was
running. In the ROM window the ROM drives that bit, on hardware as here.

## The games

`Header` below is whether the cart declares `$E8` itself or is known only by its
serial. A cart in the latter case is given the pair at $200000, the only window a
cart has; `eeprom::EEPROM_GAMES` is the table and `eeprom::detect` reads it
against the header.

Sources: `G` = `eeprom_i2c.c` from [Genesis Plus
GX](https://github.com/emu-russia/genesisplusgx), whose `i2c_database[]` supplies
the serials, the boards and the chip types; `H` = the cart's own header in
`test-roms/` and the local ROM directory; `D` = the manufacturer's datasheet,
cited in the chip table.

All 28 of these are in `EEPROM_GAMES`. Genesis Plus GX's names for them are the
names in its own table; a good number are Japanese and European releases whose
English titles vary, so the serial is the thing to trust and the emulator matches
on that - except the two zero-serial Micro Machines rows, which have no serial to
match and are the checksum rows `zero_serial_override` holds.

| Game | Serial | Chip | Size | Page | Mode | Board | Header |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Mega Man: The Wily Wars | `T-12046` | X24C01 | 128 B | 4 | 1 | Sega | declares (G) |
| Rockman: Mega World | `T-12053` | X24C01 | 128 B | 4 | 1 | Sega | declares (G) |
| Evander 'Real Deal' Holyfield's Boxing | `MK-1215` | X24C01 | 128 B | 4 | 1 | Sega | declares (G) |
| Greatest Heavyweights of the Ring | `MK-1228` | X24C01 | 128 B | 4 | 1 | Sega | declares (G) |
| Greatest Heavyweights of the Ring (J) | `G-5538` | X24C01 | 128 B | 4 | 1 | Sega | declares (G) |
| Greatest Heavyweights of the Ring (proto) | `PR-1993` | X24C01 | 128 B | 4 | 1 | Sega | declares (G) |
| Wonder Boy in Monster World | `G-4060` | X24C01 | 128 B | 4 | 1 | Sega | declares (G) |
| Sports Talk Baseball | `00001211` | X24C01 | 128 B | 4 | 1 | Sega | declares (G) |
| Honoo no Toukyuuji: Dodge Danpei | `00004076` | X24C01 | 128 B | 4 | 1 | Sega | declares (G) |
| Ninja Burai Densetsu | `G-4524` | X24C01 | 128 B | 4 | 1 | Sega | declares (G) |
| Game Toshokan | `00054503` | X24C01 | 128 B | 4 | 1 | Sega | declares (G) |
| Rings of Power | `T-50176` | X24C01 | 128 B | 4 | 1 | Ea | serial only (G) |
| NHLPA Hockey 93 | `T-50396` | X24C01 | 128 B | 4 | 1 | Ea | serial only (G) |
| John Madden Football 93 | `T-50446` | X24C01 | 128 B | 4 | 1 | Ea | serial only (G) |
| John Madden Football 93 (Champ. Ed.) | `T-50516` | X24C01 | 128 B | 4 | 1 | Ea | serial only (G) |
| Bill Walsh College Football | `T-50606` | X24C01 | 128 B | 4 | 1 | Ea | serial only (G) |
| Frank Thomas Big Hurt Baseball | `T-81476` | AT24C65 | 8 KB | 64 | 3 | Acclaim32M | declares (G,H) |
| College Slam | `T-81576` | AT24C65 | 8 KB | 64 | 3 | Acclaim32M | declares (G,H) |
| NBA Jam (J) | `T-81033` | X24C02 | 256 B | 4 | 2 | Acclaim16M | declares (G) |
| NBA Jam (UE) | `T-081326` | X24C02 | 256 B | 4 | 2 | Acclaim16M | declares (G) |
| NFL Quarterback Club | `T-081276` | AT24C02 | 256 B | 8 | 2 | Acclaim32M | declares (G) |
| NBA Jam TE | `T-81406` | AT24C04 | 512 B | 16 | 2 | Acclaim32M | declares (G) |
| NFL Quarterback Club '96 | `T-081586` | AT24C16 | 2 KB | 16 | 2 | Acclaim32M | declares (G) |
| Micro Machines 2 - Turbo Tournament | `T-120096` | AT24C16 | 2 KB | 16 | 2 | Codemasters | serial only (G) |
| Brian Lara Cricket | `T-120106` | AT24C08 | 1 KB | 16 | 2 | Codemasters | serial only (G) |
| Brian Lara Cricket 96 / Shane Warne Cricket | `T-120146` | AT24C65 | 8 KB | 64 | 3 | Codemasters | serial only (G) |
| Micro Machines: Turbo Tournament 96 (zero-serial dumps) | *(no serial)* `chk $165E` | AT24C16 | 2 KB | 16 | 2 | Codemasters | checksum + boot longword (G) |
| Micro Machines: Military (zero-serial dumps) | *(no serial)* `chk $168B` | AT24C08 | 1 KB | 16 | 2 | Codemasters | checksum + boot longword (G) |

`known_game_serial_enables_eeprom` covers the three states that matter: a cart
that only says `$E8` (Sega board, X24C01, what every Sega-board entry in the
table has), a cart that says `$E8` and is in the table for a different board and
chip, and a cart that says nothing at all and is in the table.

`eeprom_override` matches a substring of the header serial on purpose: the header
carries the `"GM "` software type and a `"-00"` revision either side of the part
that names the cart.

The Codemasters rows are the ones the header cannot name: none of the family
declares `$E8` (every extra-memory field in it is spaces), and the two Micro
Machines carts GPGX lists by checksum carry the literal serial `00000000`, so
the serial list cannot hold them. `zero_serial_override` matches GPGX's pair -
the header checksum, plus the image's first longword, which for these two carts
is `$444e4c44` - and the longword is what makes the match safe: the original
Micro Machines (Europe) revisions carry the same checksums over a different
first longword and are ordinary carts, so GPGX's checksum half alone would
catch carts that have nothing to do with the board. Its own `$165E`/
`$fffffffc` row ("Pete Sampras Tennis (Prototype ?)") is not copied for
exactly that reason. The Pete Sampras Tennis carts that GPGX lists by serial
(`T-120066`, `T-123456`) have NO_EEPROM: they carry the J-CART register with
no chip, and their serials live in `jcart.rs`'s table for the register alone.

## The chips

Mode is what the first byte of a transfer means. Mode 1 puts the word address
*in* it - seven bits of address and the read/write flag - which is what a Xicor
1 Kbit part with no device address does; mode 2 puts a device address there
(`1010` and the chip's own address pins) with a whole word-address byte behind
it; mode 3 puts a device address there with the word address behind it as **two**
bytes, high half first, of which the array answers to only the low ones - the
Atmel AT24C65 datasheet (doc2631, "Byte Write": "a write operation requires two
8-bit data word addresses following the device address word") and Genesis Plus
GX's `i2c_specs` for the 24C32 through the 24C512, all of which take
`address_bits` 16. A bit of those two bytes above the array is don't-care: the
24C65 answers to 13 of the 16 and ignores the top three of the high byte, which
is the `size_mask` half of GPGX's triple - its `GET_WORD_ADR_HIGH` never latches
a bit above it, and MAME's `i2cmem` masks the composed address with the array
size - and on the wire that means a high address byte with garbage on top is the
same address as without it. A mode-3 part's device pins name the chip on the
board instead and nothing of the address rides on them; the pins standing in for
the top of the word address is mode 2's trick, for the 24C04-16 only. The two
address bytes are a write's as much as a read's, and every byte of the sequence -
device, high, low, data - takes its own acknowledge.

The numbers are Xicor's `T7`-`T9` and timing `T15` for the X24C01 and
chapters 5-7 of Atmel's `doc0180` for the AT24C01-16 family, and they are the
`{address_bits, size_mask, pagewrite_mask}` triples of Genesis Plus GX's
`i2c_specs[]` to the byte.

| `Chip` | Size | Page | Mode | Where the numbers come from |
| --- | --- | --- | --- | --- |
| `X24C01` | 128 B | 4 | 1 | Xicor X24C01 datasheet: "Internally Organized 128 x 8", "Four Byte Page Write Mode", "a start condition followed by a seven bit word address and a write bit"; `i2c_specs` `{7, 0x7F, 0x03}` |
| `X24C02` | 256 B | 4 | 2 | Xicor X24C02 datasheet: "Internally Organized 256 x 8", "Four Byte Page Write Operation", "For the X24C02 this is fixed as 1010"; `i2c_specs` `{8, 0xFF, 0x03}` |
| `At24c01` | 128 B | 8 | 2 | Atmel doc0180: 1 Kbit, page 8; `i2c_specs` `{8, 0x7F, 0x07}` |
| `At24c02` | 256 B | 8 | 2 | Atmel doc0180: 2 Kbit, page 8; AT24C01-10SI/02-10SI "internally write organized… in pages of 8 bytes"; `i2c_specs` `{8, 0xFF, 0x07}` |
| `At24c04` | 512 B | 16 | 2 | Atmel doc0180: 4 Kbit, page 16; `i2c_specs` `{8, 0x1FF, 0x0F}` |
| `At24c08` | 1 KB | 16 | 2 | Atmel doc0180: 8 Kbit, page 16; `i2c_specs` `{8, 0x3FF, 0x0F}` |
| `At24c16` | 2 KB | 16 | 2 | Atmel doc0180: 16 Kbit, page 16; `i2c_specs` `{8, 0x7FF, 0x0F}` |
| `At24c65` | 8 KB | 64 | 3 | Atmel AT24C65 datasheet, doc2631 (the archived copy at web.archive.org, 2003-05-21, is encrypted; the two-address-byte protocol is the one GPGX and MAME's `i2cmem` both encode); `i2c_specs` `{16, 0x1FFF, 0x3F}` |

The page is what a write wraps inside: the low `log2(page)` bits of the word
address count up and the rest of the address stays where it was - Xicor's `T15`
and Atmel's "the lower order address bit inputs… are ignored during page
writes", which is the same rule from the other side. `page_write_wraps_within_page`
tests it on the X24C01's four and the AT24C02's eight. The three address pins of
a mode-2 device byte are the top of the word address on a chip big enough to
need them (Atmel doc0180, and `i2c_specs`' 9-bit mask for the 24C04), which is
what `Chip::select` is and what the second half of
`write_then_read_byte_mode2` checks; a mode-3 part's word address needs no help
from its pins, and `write_then_read_byte_mode3` walks the whole of it, the
two-byte composition, the don't-care top bits and the 64-byte page.

### Where the sources disagree

- **The X24C02's page: 4 or 8.** Genesis Plus GX's `i2c_specs[]` gives the
  X24C02 a page mask of `0x03` (4) and its 24C02 `0x07` (8), and each is right
  for its own manufacturer: Xicor's X24C02 datasheet says "Four Byte Page Write
  Operation" and Atmel's doc0180 says "8-byte Page (1K, 2K)". So the
  disagreement is between the two chips rather than between the sources, and this
  emulator follows the chip each cart is named for: `X24C02` is 4 (NBA Jam),
  `At24c02` is 8 (NFL Quarterback Club). The X24C01 and X24C02 agree with each
  other, which is the pattern the Xicor datasheets set: "The most significant
  five bits of the word address define the page address" at 128 bytes is a
  4-byte page, and it is a 4-byte page at 256 too.
- **Reading the window.** Genesis Plus GX's `mapper_i2c_generic_read8` answers an
  odd address with `eeprom_i2c_out() << sda_out_bit`: the line's own level and
  zeroes in every other bit. This emulator keeps the cart's own byte - the ROM, or
  the bank the mapper has switched in front of it - and replaces only the one bit
  the chip drives, which is what the ticket specifies
  ("SDA read-back must land in the byte read from the EEPROM window without
  disturbing any other cart address") and what
  `eeprom_keeps_priority_over_the_sram_and_mapper` pins. A game can only have
  cared about the one bit, since the other seven are the line's neighbours on
  hardware, so this is a difference in what a debug read sees rather than in what
  a game sees. Its `mapper_i2c_generic_read16` puts the line at `sda_out_bit` of a
  16-bit result instead, where this emulator's word read is its two byte reads and
  the line is in the odd half - the half the wire is on.
- **The list and the header.** Genesis Plus GX trusts `i2c_database[]` over the
  header, and marks the three carts where that matters - `T-12046`, `T-12053`,
  `G-4060`, "warning: SRAM hack exists" - with a leading space in the serial so
  that a cart with a real save RAM window keeps it. This emulator gets the same
  result the other way round: `eeprom::detect` asks `sram::detect` first and
  stands down when the header says save RAM, which is the one rule that makes
  `eeprom_and_sram_never_attach_at_once` true. The carts in the reference that
  depend on that leading space therefore get SRAM here too.
- **A one-address window with a save RAM type byte.** `eeprom_i2c_init` treats
  `0x1B2 != 0xE8` but `(sram.end - sram.start) < 2` as an EEPROM as well. Here
  only `$E8` is an EEPROM, and a cart declaring `$F8` over a single address is
  save RAM. No cart is known to declare that.

## Left out, on purpose

- **Codemasters J-CART** (`T-120106` Brian Lara Cricket, `T-120096` Micro Machines
  2 - Turbo Tournament, `T-120146` Brian Lara Cricket 96 / Shane Warne Cricket,
  and the checksum-matched Micro Machines Military and Micro Machines Turbo
  Tournament 96). Implemented since #356: the board rows are in the tables above
  and the register half of the board is `crates/genesis/src/cart/jcart.rs`. What
  this list used to say - "no J-CART cart is decoded here" - was true until the
  J-CART mapper landed; the reason it gave ("a bank-switching cart first and an
  EEPROM second ... with the bank numbers and the data line sharing the same
  registers") was not: there are no bank registers on the board, the register is
  the cart's two extra controller ports, and the shared byte is bit 7 of its odd
  read answer, which the register owns everywhere except that one bit. The chip
  of the third of these, `T-120146`, is the same mode 3 AT24C65 the two EA carts
  carry, and its serial row in `EEPROM_GAMES` is the one line #270 said it would
  be.
- **The 32 Mbit board's read-window latch.** Now implemented (see "The 32 Mbit
  board's read-window latch" above); this entry is the correction of what this
  list used to say. The earlier text called `mapper_acclaim_32M_write16` "a 64
  KB bank shift", which is not what the board does: GPGX's handler latches a
  one-bit read mux over the 1 MiB quadrant $200000-$2FFFFF (ROM, or the chip's
  /SDA line in bit 0), and the 64 KB in the old phrase is the block size of
  GPGX's memory map and of its separate `cart.hw.bankshift` flag - which is
  reset-time re-mapping shared with the SSF2 carts, not a register on this
  board. `T-081276`, `T-81406` and `T-081586` get their save and their latch.
  What this emulator still does not mirror is a *short* image across
  $300000-$3FFFFF the way GPGX's default map does (T-81406 is 3 MiB); if a cart
  is ever shown to read its own mirror, that is a mirroring issue with a name,
  not this latch.
- **Timing.** Real I2C is clocked by software with two floated lines. This samples
  on the clock's rising edge, holds the acknowledge low for the ninth clock, and
  does nothing else: no clock stretching, no bus arbitration, no pull-up model,
  no timing. `Eeprom::read` answers the chip's own level in the one bit the board
  reads back and every other bit is the cart's, which is the whole of the
  electrical story this emulator tells.
