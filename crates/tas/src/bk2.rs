//! BizHawk `.bk2` movies (#673).
//!
//! A `.bk2` is a zip (TASVideos "BK2 format", tasvideos.org/Bizhawk/BK2Format).
//! What a Genesis movie needs out of it, as BizHawk 2.8 and 2.11 write it
//! (TASVideos #5121 and #6890):
//!
//! - `Header.txt`: `Key Value` lines. `Platform GEN` is the Genesis; `Core` is
//!   `Genplus-gx`; `GameName` and `SHA1` name the ROM it was made on (the
//!   `SHA1` value of #5121 is 32 hex digits, an MD5, as the page allows:
//!   "either CRC32, MD5, or SHA1"). A movie anchored to a savestate has a core
//!   state entry in the archive and says `StartsFromSavestate True`.
//! - `SyncSettings.json`: the Genesis Plus GX core's settings. `UseSixButton`
//!   is the pad type, and `Region` is BizHawk's `LibGPGX.Region`: 0
//!   autodetect, 1 USA, 2 Europe, 3 Japan NTSC, 4 Japan PAL.
//! - `Input Log.txt`: between `[Input]` and `[/Input]`, a `LogKey:` line naming
//!   the columns, then one `|`-delimited row per frame, a character per button,
//!   `.` for "not pressed" and anything else for pressed. For Genesis Plus GX
//!   the key is `#Power|Reset|#P1 Up|P1 Down|P1 Left|P1 Right|P1 A|P1 B|P1 C|P1
//!   Start|...`, with `P1 X|P1 Y|P1 Z|P1 Mode` after Start on a 6-button pad;
//!   a `#` starts a group, and a row's groups follow the key's. Every emulated
//!   frame is a row, lag frames included, so row N is the input of frame N.
//!
//! The columns are read from the key rather than assumed, so a 3-button and a
//! 6-button log, and a core that orders them differently, read the same way.

use genesis::{Buttons, Region};

use crate::zip::Entry;
use crate::{Inputs, TasError};

/// The `Platform` a Genesis movie has.
const GENESIS: &str = "GEN";

/// Is this archive a `.bk2`: does it hold BizHawk's input log?
pub fn is_bk2(entries: &[Entry]) -> bool {
    find(entries, "Input Log.txt").is_some() && find(entries, "Header.txt").is_some()
}

/// The input of the `.bk2` whose archive entries are `entries`.
pub fn parse(entries: &[Entry]) -> Result<Inputs, TasError> {
    let header = text(entries, "Header.txt")?;
    let header = Header::parse(&header);
    match header.get("Platform") {
        Some(GENESIS) => {}
        Some(other) => {
            return Err(TasError::new(format!(
                "this BizHawk movie is for platform {other}, not the Genesis ({GENESIS})"
            )));
        }
        None => return Err(TasError::new("BizHawk movie header has no Platform")),
    }
    let anchored = header
        .get("StartsFromSavestate")
        .is_some_and(|v| v.eq_ignore_ascii_case("true"))
        || entries.iter().any(|e| is_core_state(&e.name));
    if anchored {
        return Err(TasError::new(
            "this BizHawk movie starts from a savestate, and only movies that start from power on can be played",
        ));
    }
    if header
        .get("StartsFromSaveRam")
        .is_some_and(|v| v.eq_ignore_ascii_case("true"))
    {
        return Err(TasError::new(
            "this BizHawk movie starts from saved cartridge RAM, which is not supported",
        ));
    }

    let sync = find(entries, "SyncSettings.json")
        .map(|e| String::from_utf8_lossy(&e.data).into_owned())
        .unwrap_or_default();
    let log = text(entries, "Input Log.txt")?;
    let frames = parse_log(&log)?;

    Ok(Inputs {
        frames,
        six_button: {
            let six = json_bool(&sync, "UseSixButton").unwrap_or(false);
            [six, six]
        },
        pal: None,
        region: match json_int(&sync, "Region") {
            Some(1) => Some(Region::Usa),
            Some(2 | 4) => Some(Region::Europe),
            Some(3) => Some(Region::Japan),
            _ => None,
        },
        game_name: header.get("GameName").map(str::to_string),
        rom_digest: header.get("SHA1").map(str::to_string),
        rerecords: header.get("rerecordCount").and_then(|v| v.parse().ok()),
    })
}

/// The `Input Log.txt` rows as the two pads' buttons, one entry per frame.
pub(crate) fn parse_log(log: &str) -> Result<Vec<[Buttons; 2]>, TasError> {
    let mut key: Option<Vec<Vec<Column>>> = None;
    let mut frames = Vec::new();
    for (index, line) in log.lines().enumerate() {
        let line = line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix("LogKey:") {
            key = Some(parse_key(rest)?);
            continue;
        }
        if !line.starts_with('|') {
            continue;
        }
        let key = key
            .as_ref()
            .ok_or_else(|| TasError::new("BizHawk input log has rows before its LogKey"))?;
        let frame = frames.len();
        let row = line.trim_start_matches('|').trim_end_matches('|');
        let groups: Vec<&str> = row.split('|').collect();
        if groups.len() != key.len() {
            return Err(TasError::new(format!(
                "input log line {}: {} groups where the LogKey has {}",
                index + 1,
                groups.len(),
                key.len()
            )));
        }
        let mut pads = [Buttons::empty(); 2];
        for (group, columns) in groups.iter().zip(key) {
            let marks: Vec<char> = group.chars().collect();
            if marks.len() != columns.len() {
                return Err(TasError::new(format!(
                    "input log line {}: a group of {} buttons where the LogKey has {}",
                    index + 1,
                    marks.len(),
                    columns.len()
                )));
            }
            for (mark, column) in marks.iter().zip(columns) {
                if *mark == '.' {
                    continue;
                }
                match column {
                    Column::Pad(pad, button) => pads[*pad] |= *button,
                    Column::Power if frame == 0 => {}
                    Column::Power => {
                        return Err(TasError::new(format!(
                            "the movie power-cycles the console at frame {frame}, which is not supported yet"
                        )));
                    }
                    Column::Reset => {
                        return Err(TasError::new(format!(
                            "the movie presses Reset at frame {frame}, which is not supported yet"
                        )));
                    }
                    Column::Other(name) => {
                        return Err(TasError::new(format!(
                            "the movie uses \"{name}\" at frame {frame}, which is not a Genesis pad button this player knows"
                        )));
                    }
                }
            }
        }
        frames.push(pads);
    }
    if key.is_none() {
        return Err(TasError::new("BizHawk input log has no LogKey line"));
    }
    Ok(frames)
}

/// One column of the log: a pad button, the console's own two buttons, or a
/// name this player does not know (refused if it is ever pressed).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Column {
    Pad(usize, Buttons),
    Power,
    Reset,
    Other(String),
}

/// The LogKey after `LogKey:`: groups of columns, each group started by `#`.
fn parse_key(key: &str) -> Result<Vec<Vec<Column>>, TasError> {
    let mut groups: Vec<Vec<Column>> = Vec::new();
    for name in key.split('|').filter(|name| !name.is_empty()) {
        let (starts_group, name) = match name.strip_prefix('#') {
            Some(rest) => (true, rest),
            None => (false, name),
        };
        if starts_group || groups.is_empty() {
            groups.push(Vec::new());
        }
        groups.last_mut().expect("pushed above").push(column(name));
    }
    if groups.is_empty() {
        return Err(TasError::new("BizHawk input log has an empty LogKey"));
    }
    Ok(groups)
}

/// What the column called `name` is.
fn column(name: &str) -> Column {
    match name {
        "Power" => return Column::Power,
        "Reset" => return Column::Reset,
        _ => {}
    }
    let pad = match name.get(..3) {
        Some("P1 ") => 0,
        Some("P2 ") => 1,
        _ => return Column::Other(name.to_string()),
    };
    let button = match &name[3..] {
        "Up" => Buttons::UP,
        "Down" => Buttons::DOWN,
        "Left" => Buttons::LEFT,
        "Right" => Buttons::RIGHT,
        "A" => Buttons::A,
        "B" => Buttons::B,
        "C" => Buttons::C,
        "Start" => Buttons::START,
        "X" => Buttons::X,
        "Y" => Buttons::Y,
        "Z" => Buttons::Z,
        "Mode" => Buttons::MODE,
        _ => return Column::Other(name.to_string()),
    };
    Column::Pad(pad, button)
}

/// `Header.txt`: `Key Value` lines, the value being the rest of the line.
struct Header(Vec<(String, String)>);

impl Header {
    fn parse(text: &str) -> Self {
        Self(
            text.lines()
                .filter_map(|line| {
                    let line = line.trim();
                    let (key, value) = line.split_once(' ').unwrap_or((line, ""));
                    (!key.is_empty()).then(|| (key.to_string(), value.trim().to_string()))
                })
                .collect(),
        )
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// The entry called `name` (matched without regard to case or directories).
fn find<'a>(entries: &'a [Entry], name: &str) -> Option<&'a Entry> {
    entries.iter().find(|e| {
        e.name
            .rsplit('/')
            .next()
            .is_some_and(|base| base.eq_ignore_ascii_case(name))
    })
}

/// The entry called `name`, as text.
fn text(entries: &[Entry], name: &str) -> Result<String, TasError> {
    find(entries, name)
        .map(|e| String::from_utf8_lossy(&e.data).into_owned())
        .ok_or_else(|| TasError::new(format!("BizHawk movie has no {name}")))
}

/// A core savestate entry: what makes a `.bk2` savestate-anchored. BizHawk
/// names it after the state it is (`Core.bin`, or `CoreState` with an
/// extension); `BizState` and `BizVersion` are the archive's own version files.
fn is_core_state(name: &str) -> bool {
    let base = name.rsplit('/').next().unwrap_or(name);
    base.starts_with("Core") && !base.starts_with("Comments")
}

/// `"key":true` or `"key":false` in a flat JSON object, without a JSON parser:
/// the two values this reads are a bool and a small integer, and a missing or
/// unreadable one falls back to the default.
fn json_bool(json: &str, key: &str) -> Option<bool> {
    let rest = json_value(json, key)?;
    if rest.starts_with("true") {
        Some(true)
    } else if rest.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

/// `"key":123` in a flat JSON object.
fn json_int(json: &str, key: &str) -> Option<i64> {
    let rest = json_value(json, key)?;
    let digits: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    digits.parse().ok()
}

/// The text after `"key":`, leading spaces skipped.
fn json_value<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\"");
    let at = json.find(&needle)? + needle.len();
    let rest = json[at..].trim_start().strip_prefix(':')?;
    Some(rest.trim_start())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The LogKey of #5121: six-button pads on both ports.
    pub(crate) const KEY6: &str = "LogKey:#Power|Reset|#P1 Up|P1 Down|P1 Left|P1 Right|P1 A|P1 B|P1 C|P1 Start|P1 X|P1 Y|P1 Z|P1 Mode|#P2 Up|P2 Down|P2 Left|P2 Right|P2 A|P2 B|P2 C|P2 Start|P2 X|P2 Y|P2 Z|P2 Mode|";
    /// The LogKey of #6890: three-button pads.
    const KEY3: &str = "LogKey:#Power|Reset|#P1 Up|P1 Down|P1 Left|P1 Right|P1 A|P1 B|P1 C|P1 Start|#P2 Up|P2 Down|P2 Left|P2 Right|P2 A|P2 B|P2 C|P2 Start|";

    fn entries(header: &str, sync: &str, log: &str) -> Vec<Entry> {
        vec![
            Entry {
                name: "Header.txt".into(),
                data: header.as_bytes().to_vec(),
            },
            Entry {
                name: "SyncSettings.json".into(),
                data: sync.as_bytes().to_vec(),
            },
            Entry {
                name: "Input Log.txt".into(),
                data: log.as_bytes().to_vec(),
            },
        ]
    }

    const HEADER: &str = "MovieVersion BizHawk v2.0\nPlatform GEN\nGameName Sonic The Hedgehog (W) (REV01) [!]\nSHA1 09DADB5071EB35050067A32462E39C5F\nCore Genplus-gx\nrerecordCount 9743\n";
    const SYNC6: &str = r#"{"o":{"$type":"BizHawk...GPGXSyncSettings","UseSixButton":true,"ControlTypeLeft":1,"Region":0}}"#;

    #[test]
    fn a_six_button_log_reads_by_its_key() {
        let log = format!(
            "[Input]\n{KEY6}\n|..|............|............|\n|..|.......S....|A...........|\n|..|U.....C...Z.|...........M|\n[/Input]\n"
        );
        let inputs = parse(&entries(HEADER, SYNC6, &log)).unwrap();
        assert_eq!(inputs.frames.len(), 3);
        assert_eq!(inputs.frames[0], [Buttons::empty(), Buttons::empty()]);
        assert_eq!(inputs.frames[1], [Buttons::START, Buttons::UP]);
        assert_eq!(
            inputs.frames[2],
            [Buttons::UP | Buttons::C | Buttons::Z, Buttons::MODE]
        );
        assert_eq!(inputs.six_button, [true, true]);
        assert_eq!(inputs.region, None, "Region 0 is autodetect");
        assert_eq!(inputs.rerecords, Some(9743));
        assert_eq!(
            inputs.rom_digest.as_deref(),
            Some("09DADB5071EB35050067A32462E39C5F")
        );
    }

    /// Any character but `.` is pressed: BizHawk writes the button's mnemonic,
    /// but the format does not promise which one.
    #[test]
    fn any_mark_but_a_dot_is_pressed() {
        let log = format!("{KEY3}\n|..|xxxxxxxx|........|\n");
        let frames = parse_log(&log).unwrap();
        let all = Buttons::UP
            | Buttons::DOWN
            | Buttons::LEFT
            | Buttons::RIGHT
            | Buttons::A
            | Buttons::B
            | Buttons::C
            | Buttons::START;
        assert_eq!(frames[0][0], all);
    }

    #[test]
    fn the_region_comes_from_the_sync_settings() {
        let log = format!("{KEY3}\n|..|........|........|\n");
        for (n, want) in [
            (1, Region::Usa),
            (2, Region::Europe),
            (3, Region::Japan),
            (4, Region::Europe),
        ] {
            let sync = format!(r#"{{"o":{{"UseSixButton":false,"Region":{n}}}}}"#);
            assert_eq!(
                parse(&entries(HEADER, &sync, &log)).unwrap().region,
                Some(want),
                "Region {n}"
            );
        }
    }

    #[test]
    fn power_on_frame_zero_is_the_boot_and_reset_is_refused() {
        let ok = format!("{KEY3}\n|P.|........|........|\n|..|........|........|\n");
        assert_eq!(parse_log(&ok).unwrap().len(), 2);
        let reset = format!("{KEY3}\n|..|........|........|\n|.r|........|........|\n");
        assert!(
            parse_log(&reset)
                .unwrap_err()
                .to_string()
                .contains("Reset at frame 1")
        );
        let power = format!("{KEY3}\n|..|........|........|\n|P.|........|........|\n");
        assert!(parse_log(&power).is_err());
    }

    #[test]
    fn other_platforms_and_savestate_movies_are_refused() {
        let log = format!("{KEY3}\n|..|........|........|\n");
        let nes = HEADER.replace("Platform GEN", "Platform NES");
        assert!(
            parse(&entries(&nes, "", &log))
                .unwrap_err()
                .to_string()
                .contains("NES")
        );
        let anchored = format!("{HEADER}StartsFromSavestate True\n");
        assert!(parse(&entries(&anchored, "", &log)).is_err());
        let mut with_state = entries(HEADER, "", &log);
        with_state.push(Entry {
            name: "Core.bin".into(),
            data: vec![1, 2, 3],
        });
        assert!(parse(&with_state).is_err());
    }

    #[test]
    fn a_row_that_does_not_fit_the_key_is_refused() {
        assert!(parse_log(&format!("{KEY3}\n|..|.......|........|\n")).is_err());
        assert!(parse_log(&format!("{KEY3}\n|..|........|\n")).is_err());
        assert!(parse_log("|..|........|\n").is_err(), "rows before the key");
        assert!(parse_log("[Input]\n[/Input]\n").is_err(), "no key at all");
    }

    #[test]
    fn a_pressed_column_this_player_does_not_know_is_refused() {
        let key = "LogKey:#P1 Up|P1 Trigger|";
        assert!(
            parse_log(&format!("{key}\n|..|\n")).is_ok(),
            "unknown but never pressed"
        );
        assert!(parse_log(&format!("{key}\n|.T|\n")).is_err());
    }
}
