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
| `Codemasters` (J-CART, not in the emulator) | D1 | D0 | **D7** | either byte |

Genesis Plus GX routes byte writes to the odd byte on the Sega and EA boards and
to either byte on the 16 Mbit board, which is the `Lane` field of `Pin`; the 32
Mbit board splits them, `mapper_acclaim_32M_write8` taking `/SCL` from the even
byte and `/SDA` from the odd one.

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
  `mapper_acclaim_32M_write16` is that cart's **bank shift**, which this
  emulator does not do at all yet (see "Left out"). Byte writes drive the lines
  as they should.

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

All 21 of these are in `EEPROM_GAMES`. Genesis Plus GX's names for them are the
names in its own table; a good number are Japanese and European releases whose
English titles vary, so the serial is the thing to trust and the emulator matches
on that.

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
| NBA Jam (J) | `T-81033` | X24C02 | 256 B | 4 | 2 | Acclaim16M | declares (G) |
| NBA Jam (UE) | `T-081326` | X24C02 | 256 B | 4 | 2 | Acclaim16M | declares (G) |
| NFL Quarterback Club | `T-081276` | AT24C02 | 256 B | 8 | 2 | Acclaim32M | declares (G) |
| NBA Jam TE | `T-81406` | AT24C04 | 512 B | 16 | 2 | Acclaim32M | declares (G) |
| NFL Quarterback Club '96 | `T-081586` | AT24C16 | 2 KB | 16 | 2 | Acclaim32M | declares (G) |

`known_game_serial_enables_eeprom` covers the three states that matter: a cart
that only says `$E8` (Sega board, X24C01, what every Sega-board entry in the
table has), a cart that says `$E8` and is in the table for a different board and
chip, and a cart that says nothing at all and is in the table.

`eeprom_override` matches a substring of the header serial on purpose: the header
carries the `"GM "` software type and a `"-00"` revision either side of the part
that names the cart.

## The chips

Mode is what the first byte of a transfer means. Mode 1 puts the word address
*in* it - seven bits of address and the read/write flag - which is what a Xicor
1 Kbit part with no device address does; mode 2 puts a device address there
(`1010` and the chip's own address pins) with a whole word-address byte behind
it. The numbers are Xicor's `T7`-`T9` and timing `T15` for the X24C01 and
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

The page is what a write wraps inside: the low `log2(page)` bits of the word
address count up and the rest of the address stays where it was - Xicor's `T15`
and Atmel's "the lower order address bit inputs… are ignored during page
writes", which is the same rule from the other side. `page_write_wraps_within_page`
tests it on the X24C01's four and the AT24C02's eight. The three address pins of
a mode-2 device byte are the top of the word address on a chip big enough to
need them (Atmel doc0180, and `i2c_specs`' 9-bit mask for the 24C04), which is
what `Chip::select` is and what the second half of
`write_then_read_byte_mode2` checks.

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
  Tournament 96). `Board::Codemasters` is in the board table and its bit
  positions are written down above, because they are cheap to record and cost
  something to rediscover, but no J-CART cart is decoded here and the
  `EEPROM_GAMES` table has no entries for them. The reason is that the J-CART is
  a bank-switching cart first and an EEPROM second: writes are at $300000-$37FFFF
  and reads at $380000-$3FFFFF, with the bank numbers and the data line sharing
  the same registers (`mapper_i2c_jcart_read8` ORs the line into
  `jcart_read()`). It needs the J-CART mapper to exist first, and to agree with
  it about those registers. That is worth its own ticket, and the row above is
  here - in this file, not in `eeprom::Board`, which has only the four boards the
  emulator decodes - so the bit positions are written down once.
- **Mode 3.** Genesis Plus GX's `i2c_specs` for the 24C32 up to the 24C512 - and
  for the 24C65 of `T-81476` (Frank Thomas Big Hurt Baseball), `T-81576`
  (College Slam) and `T-120146` - take `address_bits` 16: a device byte, then a
  word address in *two* bytes, with the device pins standing in for the top bits
  when the array is smaller than the addressing. `Chip` and the state machine
  have one word-address byte and no shape for that, so those carts are out. It is
  a small extension to `accept`/`select` when a cart needs it.
- **The 32 Mbit board's bank shift.** See "Word accesses are two byte accesses"
  above: `mapper_acclaim_32M_write16` is a 64 KB bank shift on those carts, and
  the shift itself is not implemented. `T-081276`, `T-81406` and `T-081586` get
  their save, and do not get the shift.
- **Timing.** Real I2C is clocked by software with two floated lines. This samples
  on the clock's rising edge, holds the acknowledge low for the ninth clock, and
  does nothing else: no clock stretching, no bus arbitration, no pull-up model,
  no timing. `Eeprom::read` answers the chip's own level in the one bit the board
  reads back and every other bit is the cart's, which is the whole of the
  electrical story this emulator tells.
