//! OSC 52 extraction from a terminal byte stream.
//!
//! OSC 52 is the escape sequence applications (and tmux) use to copy
//! text into the terminal's clipboard. Every major terminal engine
//! implements it except VTE, which refuses on principle — so the SSH
//! pane proxy scans the child's output with [`Scanner`] and lifts
//! copies out itself. The stream is only observed, never modified:
//! rendering can not be corrupted by a scanner bug.
//!
//! Grammar: `ESC ] 52 ; <targets> ; <base64-text> (BEL | ESC \)`.

/// Longer payloads are runaway sequences, not copies (tmux sends at
/// most a few hundred kilobytes); collection is abandoned past this.
const MAX_PAYLOAD: usize = 8 * 1024 * 1024;

/// OSC parameter numbers are short; anything longer is not for us.
const MAX_PARAM: usize = 8;

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;

enum State {
    /// Ordinary output; looking for `ESC`.
    Ground,
    /// After `ESC`; `]` opens an OSC.
    Escape,
    /// Collecting the OSC parameter up to `;` — only `52` matters.
    Param(Vec<u8>),
    /// Skipping the target list (`c`, `p`, …) up to `;`.
    Targets,
    /// Collecting the base64 payload up to `BEL` or `ESC \`.
    Payload { data: Vec<u8>, esc: bool },
    /// An OSC we do not care about; waiting for its terminator.
    Skip { esc: bool },
}

/// Incremental scanner; sequences may span any chunk boundary.
pub struct Scanner {
    state: State,
}

impl Default for Scanner {
    fn default() -> Self {
        Self {
            state: State::Ground,
        }
    }
}

impl Scanner {
    /// Feed the next chunk of terminal output; returns the decoded
    /// text of every complete OSC 52 copy it contained. Queries (`?`),
    /// empty payloads, and invalid base64 yield nothing.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<Vec<u8>> {
        let mut copies = Vec::new();
        for &byte in chunk {
            self.state = match std::mem::replace(&mut self.state, State::Ground) {
                State::Ground => match byte {
                    ESC => State::Escape,
                    _ => State::Ground,
                },
                State::Escape => match byte {
                    b']' => State::Param(Vec::new()),
                    ESC => State::Escape,
                    _ => State::Ground,
                },
                State::Param(mut param) => match byte {
                    b';' if param == b"52" => State::Targets,
                    b';' => State::Skip { esc: false },
                    BEL => State::Ground,
                    ESC => State::Skip { esc: true },
                    _ if param.len() >= MAX_PARAM => State::Skip { esc: false },
                    _ => {
                        param.push(byte);
                        State::Param(param)
                    }
                },
                State::Targets => match byte {
                    b';' => State::Payload {
                        data: Vec::new(),
                        esc: false,
                    },
                    BEL => State::Ground,
                    ESC => State::Skip { esc: true },
                    _ => State::Targets,
                },
                State::Payload { mut data, esc } => match (esc, byte) {
                    (true, b'\\') => {
                        if let Some(text) = complete(&data) {
                            copies.push(text);
                        }
                        State::Ground
                    }
                    // ESC followed by anything else: malformed, drop it
                    (true, _) => State::Ground,
                    (false, BEL) => {
                        if let Some(text) = complete(&data) {
                            copies.push(text);
                        }
                        State::Ground
                    }
                    (false, ESC) => State::Payload { data, esc: true },
                    (false, _) if data.len() >= MAX_PAYLOAD => State::Skip { esc: false },
                    (false, _) => {
                        data.push(byte);
                        State::Payload { data, esc: false }
                    }
                },
                State::Skip { esc } => match (esc, byte) {
                    (true, b'\\') => State::Ground,
                    (true, _) => State::Skip { esc: false },
                    (false, BEL) => State::Ground,
                    (false, ESC) => State::Skip { esc: true },
                    (false, _) => State::Skip { esc: false },
                },
            };
        }
        copies
    }
}

/// A finished payload: `?` is a clipboard *read* — never answered,
/// deliberately — and an empty copy must not clobber the clipboard.
fn complete(payload: &[u8]) -> Option<Vec<u8>> {
    if payload == b"?" {
        return None;
    }
    decode_base64(payload).filter(|text| !text.is_empty())
}

/// Standard-alphabet base64. `None` on any foreign byte — corrupt
/// payloads are dropped, never half-decoded onto the clipboard.
fn decode_base64(input: &[u8]) -> Option<Vec<u8>> {
    let digit = |byte: u8| -> Option<u32> {
        match byte {
            b'A'..=b'Z' => Some(u32::from(byte - b'A')),
            b'a'..=b'z' => Some(u32::from(byte - b'a') + 26),
            b'0'..=b'9' => Some(u32::from(byte - b'0') + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    };
    let mut end = input.len();
    while end > 0 && input.len() - end < 2 && input[end - 1] == b'=' {
        end -= 1;
    }
    let mut out = Vec::with_capacity(end * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for &byte in &input[..end] {
        acc = (acc << 6) | digit(byte)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(chunks: &[&[u8]]) -> Vec<Vec<u8>> {
        let mut scanner = Scanner::default();
        chunks
            .iter()
            .flat_map(|chunk| scanner.feed(chunk))
            .collect()
    }

    #[test]
    fn plain_output_yields_nothing() {
        assert!(feed_all(&[b"hello\r\nworld"]).is_empty());
    }

    #[test]
    fn bel_terminated_copy_is_decoded() {
        // "hello" = aGVsbG8=
        assert_eq!(feed_all(&[b"\x1b]52;c;aGVsbG8=\x07"]), [b"hello".to_vec()]);
    }

    #[test]
    fn st_terminated_copy_is_decoded() {
        assert_eq!(
            feed_all(&[b"\x1b]52;c;aGVsbG8=\x1b\\"]),
            [b"hello".to_vec()]
        );
    }

    #[test]
    fn empty_target_list_still_parses() {
        // tmux sends `52;;` when no explicit targets are configured
        assert_eq!(feed_all(&[b"\x1b]52;;aGVsbG8=\x07"]), [b"hello".to_vec()]);
    }

    #[test]
    fn sequences_survive_any_chunk_boundary() {
        let stream = b"before \x1b]52;c;aGVsbG8=\x07 after";
        for split in 0..stream.len() {
            let (a, b) = stream.split_at(split);
            assert_eq!(feed_all(&[a, b]), [b"hello".to_vec()], "split at {split}");
        }
    }

    #[test]
    fn byte_at_a_time_feeding_works() {
        let stream: Vec<&[u8]> = b"\x1b]52;c;aGVsbG8=\x1b\\".chunks(1).collect();
        assert_eq!(feed_all(&stream), [b"hello".to_vec()]);
    }

    #[test]
    fn other_osc_sequences_are_ignored() {
        assert!(feed_all(&[b"\x1b]0;window title\x07"]).is_empty());
        assert!(feed_all(&[b"\x1b]7;file:///home/e\x1b\\"]).is_empty());
        // "520" must not match a "52" prefix
        assert!(feed_all(&[b"\x1b]520;c;aGVsbG8=\x07"]).is_empty());
    }

    #[test]
    fn clipboard_queries_are_never_answered() {
        assert!(feed_all(&[b"\x1b]52;c;?\x07"]).is_empty());
    }

    #[test]
    fn invalid_base64_is_dropped() {
        assert!(feed_all(&[b"\x1b]52;c;not base64!\x07"]).is_empty());
        assert!(feed_all(&[b"\x1b]52;c;aG=sbG8=\x07"]).is_empty());
    }

    #[test]
    fn empty_copies_are_dropped() {
        assert!(feed_all(&[b"\x1b]52;c;\x07"]).is_empty());
    }

    #[test]
    fn multiple_copies_in_one_chunk() {
        assert_eq!(
            feed_all(&[b"\x1b]52;c;YQ==\x07 mid \x1b]52;c;Yg==\x07"]),
            [b"a".to_vec(), b"b".to_vec()]
        );
    }

    #[test]
    fn scanning_resumes_after_an_oversized_payload() {
        let mut huge = b"\x1b]52;c;".to_vec();
        huge.extend(std::iter::repeat_n(b'A', MAX_PAYLOAD + 8));
        huge.push(0x07);
        huge.extend_from_slice(b"\x1b]52;c;aGVsbG8=\x07");
        assert_eq!(feed_all(&[&huge]), [b"hello".to_vec()]);
    }

    #[test]
    fn esc_inside_payload_aborts_cleanly() {
        assert!(feed_all(&[b"\x1b]52;c;aGVs\x1bXbG8=\x07"]).is_empty());
        // and the scanner is back in ground state afterwards
        assert_eq!(
            feed_all(&[b"\x1b]52;c;aG\x1bX\x07\x1b]52;c;aGVsbG8=\x07"]),
            [b"hello".to_vec()]
        );
    }

    #[test]
    fn base64_padding_variants_decode() {
        assert_eq!(decode_base64(b"YQ=="), Some(b"a".to_vec()));
        assert_eq!(decode_base64(b"YWI="), Some(b"ab".to_vec()));
        assert_eq!(decode_base64(b"YWJj"), Some(b"abc".to_vec()));
        assert_eq!(decode_base64(b""), Some(Vec::new()));
        assert_eq!(decode_base64(b"===="), None);
    }
}
