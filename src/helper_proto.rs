use serde::Deserialize;
use std::time::{Duration, Instant};

// Wire types and validation for the helper protocol (docs/helper-protocol.md,
// v1). Everything arriving on the socket is untrusted input into a
// root-adjacent process: parsing is serde into fixed shapes, bounds are
// enforced here, and the caller never sees an unsanitized state.

pub const MAX_LINE_BYTES: usize = 4096;
pub const MAX_GROUPS: usize = 4;
pub const MAX_WS_PER_GROUP: usize = 16;
pub const MAX_WS_IDX: u8 = 32;
pub const MAX_CLAUDE_SESSIONS: usize = 8;
pub const MAX_SESSION_ID_LEN: usize = 64;
pub const MAX_MEDIA_TITLE_CHARS: usize = 96;
pub const MAX_MEDIA_ARTIST_CHARS: usize = 80;
pub const MAX_MEDIA_ART_PATH_CHARS: usize = 512;
pub const SUPPORTED_VERSION: u64 = 1;
const MAX_CONSECUTIVE_INVALID: u32 = 8;
const MAX_MSGS_PER_WINDOW: u32 = 200;
const RATE_WINDOW: Duration = Duration::from_secs(1);

#[derive(Debug, Deserialize)]
#[serde(tag = "t")]
pub enum HelperMessage {
    #[serde(rename = "hello")]
    Hello { v: u64 },
    #[serde(rename = "state")]
    State(StateMsg),
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct StateMsg {
    #[serde(default)]
    pub outs: Vec<OutputGroup>,
    #[serde(default)]
    pub vol: Option<Vol>,
    // Pet-Claude presence: one critter per running Claude Code session.
    #[serde(default)]
    pub claude: Option<ClaudePresence>,
    // Render-only now-playing metadata. The helper owns MPRIS/user-session
    // access; the daemon only paints this when the helper state is fresh.
    #[serde(default)]
    pub media: Option<NowPlaying>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct ClaudePresence {
    #[serde(default)]
    pub sessions: Vec<ClaudeSession>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct ClaudeSession {
    #[serde(default)]
    pub id: String,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct NowPlaying {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub art_path: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct OutputGroup {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub ws: Vec<WsEntry>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
pub struct WsEntry {
    pub id: u64,
    pub idx: u8,
    #[serde(default)]
    pub occ: bool,
    #[serde(default)]
    pub foc: bool,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
pub struct Vol {
    pub level: f64,
    #[serde(default)]
    pub muted: bool,
}

pub fn parse_line(line: &str) -> Result<HelperMessage, serde_json::Error> {
    serde_json::from_str(line)
}

// Enforce the documented bounds on a parsed state: at most MAX_GROUPS
// groups of MAX_WS_PER_GROUP entries, idx within 1..=MAX_WS_IDX, a single
// focused workspace (first wins), volume clamped to [0, 1]. JSON cannot
// carry NaN/Infinity, so parsed floats are always finite.
pub fn sanitize_state(mut state: StateMsg) -> StateMsg {
    state.outs.truncate(MAX_GROUPS);
    let mut seen_focused = false;
    for group in &mut state.outs {
        group.ws.truncate(MAX_WS_PER_GROUP);
        group.ws.retain(|w| (1..=MAX_WS_IDX).contains(&w.idx));
        for entry in &mut group.ws {
            if entry.foc {
                if seen_focused {
                    entry.foc = false;
                } else {
                    seen_focused = true;
                }
            }
        }
    }
    // Empty groups must not survive: a divider never borders an empty group,
    // and a non-empty model that renders zero buttons would break the strip
    // updater's fixed point (classify would see it as structurally new on
    // every heartbeat, draining touches forever).
    state.outs.retain(|group| !group.ws.is_empty());
    if let Some(vol) = &mut state.vol {
        vol.level = vol.level.clamp(0.0, 1.0);
    }
    if let Some(claude) = &mut state.claude {
        claude.sessions.truncate(MAX_CLAUDE_SESSIONS);
        for session in &mut claude.sessions {
            if session.id.len() > MAX_SESSION_ID_LEN {
                session.id = session.id.chars().take(MAX_SESSION_ID_LEN).collect();
            }
        }
    }
    if let Some(media) = &mut state.media {
        media.title = clean_media_text(&media.title, MAX_MEDIA_TITLE_CHARS);
        media.artist = clean_media_text(&media.artist, MAX_MEDIA_ARTIST_CHARS);
        media.art_path = media.art_path.take().and_then(clean_media_art_path);
        if media.title.is_empty() {
            state.media = None;
        }
    }
    state
}

fn clean_media_text(value: &str, max_chars: usize) -> String {
    value
        .trim()
        .chars()
        .filter(|ch| !ch.is_control())
        .take(max_chars)
        .collect()
}

fn clean_media_art_path(value: String) -> Option<String> {
    let value = value.trim();
    if value.len() > MAX_MEDIA_ART_PATH_CHARS
        || !value.ends_with(".png")
        || !value.starts_with("/run/tiny-dfr-ben/media/")
    {
        return None;
    }
    Some(value.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Intent {
    SetVolume(f64),
    FocusWorkspace(u64),
}

pub fn encode_intent(intent: &Intent) -> String {
    match intent {
        Intent::SetVolume(level) => {
            let level = if level.is_finite() {
                level.clamp(0.0, 1.0)
            } else {
                0.0
            };
            format!("{{\"t\":\"set-volume\",\"level\":{level:.4}}}\n")
        }
        Intent::FocusWorkspace(id) => {
            format!("{{\"t\":\"focus-workspace\",\"id\":{id}}}\n")
        }
    }
}

// Reassembles NDJSON lines from arbitrary read chunks; any line (terminated
// or still accumulating) longer than MAX_LINE_BYTES is a protocol violation
// and the connection must be dropped.
#[derive(Default)]
pub struct LineBuffer {
    buf: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct LineOverflow;

impl LineBuffer {
    pub fn new() -> LineBuffer {
        LineBuffer::default()
    }

    pub fn push(&mut self, data: &[u8]) -> Result<Vec<String>, LineOverflow> {
        self.buf.extend_from_slice(data);
        let mut lines = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let rest = self.buf.split_off(pos + 1);
            let mut line = std::mem::replace(&mut self.buf, rest);
            line.pop(); // the newline
            if line.len() > MAX_LINE_BYTES {
                return Err(LineOverflow);
            }
            // Invalid UTF-8 becomes a parse failure downstream, which counts
            // against the error budget rather than killing the connection.
            lines.push(String::from_utf8_lossy(&line).into_owned());
        }
        if self.buf.len() > MAX_LINE_BYTES {
            return Err(LineOverflow);
        }
        Ok(lines)
    }
}

// Per-connection error accounting: 8 consecutive invalid lines or a
// sustained message flood ends the connection.
#[derive(Default)]
pub struct ErrorBudget {
    consecutive_invalid: u32,
    window_start: Option<Instant>,
    msgs_in_window: u32,
}

impl ErrorBudget {
    pub fn new() -> ErrorBudget {
        ErrorBudget::default()
    }

    // Returns true when the invalid-line budget is exhausted.
    pub fn on_invalid(&mut self) -> bool {
        self.consecutive_invalid += 1;
        self.consecutive_invalid >= MAX_CONSECUTIVE_INVALID
    }

    pub fn on_valid(&mut self) {
        self.consecutive_invalid = 0;
    }

    // Rate accounting for every line, valid or not; true = flood, disconnect.
    pub fn on_message(&mut self, now: Instant) -> bool {
        match self.window_start {
            Some(start) if now.saturating_duration_since(start) < RATE_WINDOW => {
                self.msgs_in_window += 1;
                self.msgs_in_window > MAX_MSGS_PER_WINDOW
            }
            _ => {
                self.window_start = Some(now);
                self.msgs_in_window = 1;
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_and_state_parse() {
        let hello = parse_line(r#"{"t":"hello","v":1,"src":"tiny-dfr-helper"}"#).unwrap();
        assert!(matches!(hello, HelperMessage::Hello { v: 1 }));

        let state = parse_line(
            r#"{"t":"state","outs":[{"name":"eDP-1","ws":[{"id":7,"idx":1,"occ":true,"foc":true}]}],"vol":{"level":0.6,"muted":true}}"#,
        )
        .unwrap();
        let HelperMessage::State(state) = state else {
            panic!("expected state");
        };
        assert_eq!(state.outs.len(), 1);
        assert_eq!(state.outs[0].name, "eDP-1");
        assert_eq!(
            state.outs[0].ws[0],
            WsEntry {
                id: 7,
                idx: 1,
                occ: true,
                foc: true
            }
        );
        assert_eq!(state.vol.unwrap().level, 0.6);
        assert!(state.vol.unwrap().muted);
    }

    #[test]
    fn unknown_fields_are_ignored_unknown_types_fail() {
        let msg =
            parse_line(r#"{"t":"state","outs":[],"claude":{"sessions":[{"id":"x"}]},"future":42}"#);
        assert!(msg.is_ok());

        assert!(parse_line(r#"{"t":"reboot"}"#).is_err());
        assert!(parse_line("not json").is_err());
        assert!(parse_line(r#"{"t":"state","outs":[{"ws":[{"id":-1,"idx":1}]}]}"#).is_err());
    }

    #[test]
    fn media_state_parses_and_sanitizes() {
        let state = parse_line(
            r#"{"t":"state","outs":[],"media":{"title":"  Song\u0007 Name  ","artist":"Artist","art_path":"/run/tiny-dfr-ben/media/current.png","future":true}}"#,
        )
        .unwrap();
        let HelperMessage::State(state) = state else {
            panic!("expected state");
        };

        let state = sanitize_state(state);
        let media = state.media.unwrap();
        assert_eq!(media.title, "Song Name");
        assert_eq!(media.artist, "Artist");
        assert_eq!(
            media.art_path.as_deref(),
            Some("/run/tiny-dfr-ben/media/current.png")
        );
    }

    #[test]
    fn media_sanitizer_drops_empty_titles_and_unsafe_art_paths() {
        let state = sanitize_state(StateMsg {
            outs: vec![],
            vol: None,
            claude: None,
            media: Some(NowPlaying {
                title: "   ".to_string(),
                artist: "Artist".to_string(),
                art_path: Some("/run/tiny-dfr-ben/media/current.png".to_string()),
            }),
        });
        assert!(state.media.is_none());

        let state = sanitize_state(StateMsg {
            outs: vec![],
            vol: None,
            claude: None,
            media: Some(NowPlaying {
                title: "Track".to_string(),
                artist: "Artist".to_string(),
                art_path: Some("/home/ben/.cache/private.png".to_string()),
            }),
        });
        assert_eq!(state.media.unwrap().art_path, None);

        let state = sanitize_state(StateMsg {
            outs: vec![],
            vol: None,
            claude: None,
            media: Some(NowPlaying {
                title: "Track".to_string(),
                artist: "Artist".to_string(),
                art_path: Some("/tmp/tiny-dfr-ben/current.png".to_string()),
            }),
        });
        assert_eq!(state.media.unwrap().art_path, None);
    }

    #[test]
    fn media_sanitizer_truncates_text_fields() {
        let state = sanitize_state(StateMsg {
            outs: vec![],
            vol: None,
            claude: None,
            media: Some(NowPlaying {
                title: "t".repeat(MAX_MEDIA_TITLE_CHARS + 20),
                artist: "a".repeat(MAX_MEDIA_ARTIST_CHARS + 20),
                art_path: Some("/run/tiny-dfr-ben/media/current.jpg".to_string()),
            }),
        });

        let media = state.media.unwrap();
        assert_eq!(media.title.len(), MAX_MEDIA_TITLE_CHARS);
        assert_eq!(media.artist.len(), MAX_MEDIA_ARTIST_CHARS);
        assert_eq!(media.art_path, None);
    }

    #[test]
    fn sanitize_enforces_group_and_entry_bounds() {
        let groups = (0..6)
            .map(|g| OutputGroup {
                name: format!("out{g}"),
                ws: (0..20)
                    .map(|i| WsEntry {
                        id: (g * 100 + i) as u64,
                        idx: (i + 1) as u8,
                        occ: true,
                        foc: false,
                    })
                    .collect(),
            })
            .collect();
        let state = sanitize_state(StateMsg {
            outs: groups,
            vol: None,
            claude: None,
            media: None,
        });

        assert_eq!(state.outs.len(), MAX_GROUPS);
        assert!(state.outs.iter().all(|g| g.ws.len() <= MAX_WS_PER_GROUP));
    }

    #[test]
    fn sanitize_filters_bad_idx_and_extra_focus_and_clamps_volume() {
        let state = sanitize_state(StateMsg {
            outs: vec![OutputGroup {
                name: "eDP-1".into(),
                ws: vec![
                    WsEntry {
                        id: 1,
                        idx: 0,
                        occ: true,
                        foc: false,
                    },
                    WsEntry {
                        id: 2,
                        idx: 1,
                        occ: true,
                        foc: true,
                    },
                    WsEntry {
                        id: 3,
                        idx: 33,
                        occ: true,
                        foc: false,
                    },
                    WsEntry {
                        id: 4,
                        idx: 2,
                        occ: true,
                        foc: true,
                    },
                ],
            }],
            vol: Some(Vol {
                level: 1.35,
                muted: false,
            }),
            claude: None,
            media: None,
        });

        let ws = &state.outs[0].ws;
        assert_eq!(ws.iter().map(|w| w.id).collect::<Vec<_>>(), vec![2, 4]);
        assert_eq!(ws.iter().filter(|w| w.foc).count(), 1);
        assert!(ws[0].foc);
        assert_eq!(state.vol.unwrap().level, 1.0);
    }

    #[test]
    fn sanitize_bounds_claude_sessions() {
        let state = sanitize_state(StateMsg {
            outs: vec![],
            vol: None,
            claude: Some(ClaudePresence {
                sessions: (0..10)
                    .map(|i| ClaudeSession {
                        id: format!("{}{}", "x".repeat(100), i),
                    })
                    .collect(),
            }),
            media: None,
        });

        let sessions = &state.claude.unwrap().sessions;
        assert_eq!(sessions.len(), MAX_CLAUDE_SESSIONS);
        assert!(sessions.iter().all(|s| s.id.len() <= MAX_SESSION_ID_LEN));
    }

    #[test]
    fn sanitize_drops_empty_groups() {
        let state = sanitize_state(StateMsg {
            outs: vec![
                OutputGroup {
                    name: "empty".into(),
                    ws: vec![],
                },
                OutputGroup {
                    name: "bad-idx-only".into(),
                    ws: vec![WsEntry {
                        id: 1,
                        idx: 0,
                        occ: true,
                        foc: false,
                    }],
                },
                OutputGroup {
                    name: "eDP-1".into(),
                    ws: vec![WsEntry {
                        id: 2,
                        idx: 1,
                        occ: true,
                        foc: false,
                    }],
                },
            ],
            vol: None,
            claude: None,
            media: None,
        });

        assert_eq!(state.outs.len(), 1);
        assert_eq!(state.outs[0].name, "eDP-1");
    }

    #[test]
    fn intents_encode_exactly() {
        assert_eq!(
            encode_intent(&Intent::SetVolume(0.42)),
            "{\"t\":\"set-volume\",\"level\":0.4200}\n"
        );
        assert_eq!(
            encode_intent(&Intent::SetVolume(f64::NAN)),
            "{\"t\":\"set-volume\",\"level\":0.0000}\n"
        );
        assert_eq!(
            encode_intent(&Intent::FocusWorkspace(9)),
            "{\"t\":\"focus-workspace\",\"id\":9}\n"
        );
    }

    #[test]
    fn line_buffer_reassembles_partial_reads() {
        let mut buf = LineBuffer::new();
        assert_eq!(buf.push(b"{\"t\":\"he").unwrap(), Vec::<String>::new());
        assert_eq!(
            buf.push(b"llo\",\"v\":1}\n{\"a\":2}\n{").unwrap(),
            vec![
                r#"{"t":"hello","v":1}"#.to_string(),
                r#"{"a":2}"#.to_string()
            ]
        );
        assert_eq!(buf.push(b"}\n").unwrap(), vec!["{}".to_string()]);
    }

    #[test]
    fn line_buffer_caps_line_length() {
        let mut buf = LineBuffer::new();
        let big = vec![b'x'; MAX_LINE_BYTES + 1];
        assert_eq!(buf.push(&big), Err(LineOverflow));

        let mut buf = LineBuffer::new();
        let mut terminated = vec![b'y'; MAX_LINE_BYTES + 1];
        terminated.push(b'\n');
        assert_eq!(buf.push(&terminated), Err(LineOverflow));
    }

    #[test]
    fn error_budget_trips_on_consecutive_invalid_and_resets_on_valid() {
        let mut budget = ErrorBudget::new();
        for _ in 0..6 {
            assert!(!budget.on_invalid());
        }
        budget.on_valid();
        for _ in 0..7 {
            assert!(!budget.on_invalid());
        }
        assert!(budget.on_invalid());
    }

    #[test]
    fn error_budget_trips_on_message_flood() {
        let mut budget = ErrorBudget::new();
        let t0 = Instant::now();
        for _ in 0..200 {
            assert!(!budget.on_message(t0));
        }
        assert!(budget.on_message(t0));
        // A new window resets the counter.
        assert!(!budget.on_message(t0 + Duration::from_millis(1100)));
    }
}
