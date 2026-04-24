//! Receive-pack command-list parser.
//!
//! The body of a `POST /git-receive-pack` request starts with a
//! pkt-line-framed sequence of ref-update commands, terminated by
//! a flush packet, followed by the raw pack bytes.
//!
//! ```text
//!   pkt-line: <old-sha> SP <new-sha> SP <refname> NUL <capabilities> LF
//!   pkt-line: <old-sha> SP <new-sha> SP <refname> LF    (subsequent)
//!   ...
//!   0000                                                  (flush)
//!   PACK...                                               (pack bytes)
//! ```
//!
//! The first command carries the client's selected capabilities
//! after a NUL. Subsequent commands omit it. Shas are lowercase
//! 40-char hex. Zero-sha (40 zeros) on the old side means create;
//! zero-sha on the new side means delete; both non-zero = update.

use std::fmt;

use crate::advertise::ZERO_SHA;

/// One ref-update command parsed from the pkt-line prologue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefCommand {
    pub old_sha: String,
    pub new_sha: String,
    pub refname: String,
}

/// High-level kind of ref-update, derived from `old_sha`/`new_sha`
/// comparison against the zero-sha sentinel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandKind {
    Create,
    Delete,
    Update,
}

impl RefCommand {
    /// Classify the command by zero-sha placement.
    pub fn kind(&self) -> CommandKind {
        let old_zero = self.old_sha == ZERO_SHA;
        let new_zero = self.new_sha == ZERO_SHA;
        match (old_zero, new_zero) {
            (true, false) => CommandKind::Create,
            (false, true) => CommandKind::Delete,
            _ => CommandKind::Update,
        }
    }
}

/// Parsed command list + the input offset where the pack starts
/// (i.e. just after the flush packet).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCommands {
    pub commands: Vec<RefCommand>,
    /// Client-declared capabilities from the first command's
    /// post-NUL block. Empty vec if no NUL seen.
    pub capabilities: Vec<String>,
    /// Byte offset at which the pack stream begins (immediately
    /// after the terminating `0000` flush packet).
    pub pack_offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandsError {
    /// Buffer too short to carry a pkt-line length prefix.
    Truncated,
    /// Length prefix wasn't 4 lowercase-hex chars.
    BadLengthPrefix([u8; 4]),
    /// Declared pkt-line length exceeds the remaining buffer.
    LengthOverflow { declared: usize, remaining: usize },
    /// Command line didn't match `<40-hex> SP <40-hex> SP <ref>LF`.
    MalformedCommand(String),
    /// Command list ended without a flush packet.
    MissingFlush,
    /// No commands at all (git always sends at least one).
    EmptyCommandList,
}

impl fmt::Display for CommandsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CommandsError::Truncated => write!(f, "command stream truncated"),
            CommandsError::BadLengthPrefix(p) => write!(f, "bad pkt-line length: {:02x?}", p),
            CommandsError::LengthOverflow { declared, remaining } => write!(
                f,
                "pkt-line declared {declared} bytes, only {remaining} remain"
            ),
            CommandsError::MalformedCommand(why) => write!(f, "malformed command: {why}"),
            CommandsError::MissingFlush => write!(f, "missing flush terminator"),
            CommandsError::EmptyCommandList => write!(f, "no ref commands"),
        }
    }
}

impl std::error::Error for CommandsError {}

/// Parse the command-list prologue of a git-receive-pack request
/// body. Returns the parsed commands + the offset where the pack
/// begins.
pub fn parse_commands(buf: &[u8]) -> Result<ParsedCommands, CommandsError> {
    let mut cursor = 0usize;
    let mut commands = Vec::new();
    let mut capabilities: Vec<String> = Vec::new();

    loop {
        if cursor + 4 > buf.len() {
            return Err(CommandsError::Truncated);
        }
        let len_slice = &buf[cursor..cursor + 4];
        let len_str =
            std::str::from_utf8(len_slice).map_err(|_| {
                let mut p = [0u8; 4];
                p.copy_from_slice(len_slice);
                CommandsError::BadLengthPrefix(p)
            })?;
        let declared = usize::from_str_radix(len_str, 16).map_err(|_| {
            let mut p = [0u8; 4];
            p.copy_from_slice(len_slice);
            CommandsError::BadLengthPrefix(p)
        })?;

        if declared == 0 {
            // Flush packet — end of command list.
            cursor += 4;
            break;
        }
        if declared < 4 {
            return Err(CommandsError::MalformedCommand(format!(
                "pkt-line length {declared} < 4"
            )));
        }
        if cursor + declared > buf.len() {
            return Err(CommandsError::LengthOverflow {
                declared,
                remaining: buf.len() - cursor,
            });
        }
        let payload = &buf[cursor + 4..cursor + declared];
        cursor += declared;

        // Strip trailing LF if present.
        let payload = match payload.strip_suffix(b"\n") {
            Some(trimmed) => trimmed,
            None => payload,
        };

        // First command may carry \0<caps>.
        let (cmd_bytes, caps_bytes) = match payload.iter().position(|&b| b == 0) {
            Some(nul) => (&payload[..nul], Some(&payload[nul + 1..])),
            None => (payload, None),
        };

        let cmd_str = std::str::from_utf8(cmd_bytes)
            .map_err(|_| CommandsError::MalformedCommand("non-UTF-8 command".into()))?;
        let parts: Vec<&str> = cmd_str.splitn(3, ' ').collect();
        if parts.len() != 3 {
            return Err(CommandsError::MalformedCommand(format!(
                "expected 3 space-separated fields, got {}: {:?}",
                parts.len(),
                cmd_str
            )));
        }
        validate_sha(parts[0])?;
        validate_sha(parts[1])?;
        validate_refname(parts[2])?;

        commands.push(RefCommand {
            old_sha: parts[0].to_string(),
            new_sha: parts[1].to_string(),
            refname: parts[2].to_string(),
        });

        if let Some(caps_bytes) = caps_bytes
            && capabilities.is_empty()
        {
            let caps_str = std::str::from_utf8(caps_bytes)
                .map_err(|_| CommandsError::MalformedCommand("non-UTF-8 caps".into()))?;
            capabilities = caps_str
                .split_whitespace()
                .map(|s| s.to_string())
                .collect();
        }
    }

    if commands.is_empty() {
        return Err(CommandsError::EmptyCommandList);
    }

    Ok(ParsedCommands {
        commands,
        capabilities,
        pack_offset: cursor,
    })
}

fn validate_sha(s: &str) -> Result<(), CommandsError> {
    if s.len() != 40 || !s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
        return Err(CommandsError::MalformedCommand(format!(
            "sha must be 40 lowercase hex chars, got {:?}",
            s
        )));
    }
    Ok(())
}

fn validate_refname(name: &str) -> Result<(), CommandsError> {
    if name.is_empty() {
        return Err(CommandsError::MalformedCommand("empty refname".into()));
    }
    // Git refname rules are elaborate (git-check-ref-format); for
    // v0 we accept any non-empty, no-whitespace, no-control name.
    for c in name.chars() {
        if c.is_whitespace() || c.is_control() {
            return Err(CommandsError::MalformedCommand(format!(
                "refname contains whitespace/control: {:?}",
                name
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkt(payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + payload.len());
        out.extend_from_slice(format!("{:04x}", payload.len() + 4).as_bytes());
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn single_create_command() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&pkt(
            b"0000000000000000000000000000000000000000 \
              1111111111111111111111111111111111111111 \
              refs/heads/main\0report-status side-band-64k agent=git/2.50\n",
        ));
        buf.extend_from_slice(b"0000");
        let parsed = parse_commands(&buf).unwrap();
        assert_eq!(parsed.commands.len(), 1);
        assert_eq!(parsed.commands[0].old_sha, ZERO_SHA);
        assert_eq!(parsed.commands[0].new_sha, "1".repeat(40));
        assert_eq!(parsed.commands[0].refname, "refs/heads/main");
        assert_eq!(parsed.commands[0].kind(), CommandKind::Create);
        assert_eq!(parsed.capabilities.len(), 3);
        assert!(parsed.capabilities.contains(&"report-status".to_string()));
        assert_eq!(parsed.pack_offset, buf.len());
    }

    #[test]
    fn pack_offset_points_past_flush() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&pkt(
            b"0000000000000000000000000000000000000000 \
              2222222222222222222222222222222222222222 \
              refs/heads/main\0report-status\n",
        ));
        buf.extend_from_slice(b"0000");
        buf.extend_from_slice(b"PACK-bytes-follow-here");
        let parsed = parse_commands(&buf).unwrap();
        assert_eq!(&buf[parsed.pack_offset..], b"PACK-bytes-follow-here");
    }

    #[test]
    fn multiple_commands_capabilities_only_first() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&pkt(
            b"0000000000000000000000000000000000000000 \
              aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
              refs/heads/main\0report-status\n",
        ));
        buf.extend_from_slice(&pkt(
            b"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb \
              cccccccccccccccccccccccccccccccccccccccc \
              refs/heads/feature\n",
        ));
        buf.extend_from_slice(b"0000");
        let parsed = parse_commands(&buf).unwrap();
        assert_eq!(parsed.commands.len(), 2);
        assert_eq!(parsed.commands[1].kind(), CommandKind::Update);
    }

    #[test]
    fn delete_command_classified() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&pkt(
            b"dddddddddddddddddddddddddddddddddddddddd \
              0000000000000000000000000000000000000000 \
              refs/heads/stale\0report-status\n",
        ));
        buf.extend_from_slice(b"0000");
        let parsed = parse_commands(&buf).unwrap();
        assert_eq!(parsed.commands[0].kind(), CommandKind::Delete);
    }

    #[test]
    fn missing_flush_rejected() {
        let buf = pkt(
            b"0000000000000000000000000000000000000000 \
              1111111111111111111111111111111111111111 \
              refs/heads/main\n",
        );
        let err = parse_commands(&buf).unwrap_err();
        assert!(matches!(err, CommandsError::Truncated));
    }

    #[test]
    fn empty_just_flush_rejected() {
        let err = parse_commands(b"0000").unwrap_err();
        assert_eq!(err, CommandsError::EmptyCommandList);
    }

    #[test]
    fn uppercase_hex_sha_rejected() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&pkt(
            b"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA \
              1111111111111111111111111111111111111111 \
              refs/heads/main\0report-status\n",
        ));
        buf.extend_from_slice(b"0000");
        assert!(matches!(
            parse_commands(&buf).unwrap_err(),
            CommandsError::MalformedCommand(_)
        ));
    }

    #[test]
    fn whitespace_in_refname_rejected() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&pkt(
            b"0000000000000000000000000000000000000000 \
              1111111111111111111111111111111111111111 \
              refs/heads/bad\x09name\0report-status\n",
        ));
        buf.extend_from_slice(b"0000");
        assert!(matches!(
            parse_commands(&buf).unwrap_err(),
            CommandsError::MalformedCommand(_)
        ));
    }
}
