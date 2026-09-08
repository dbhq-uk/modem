//! The dialling directory: a tab-separated, hand-editable phone book -
//! the comms-package convention this project is modelled on. The point
//! of the format is that it opens in any text editor and reads like a
//! file somebody typed, not a database export, so [`Directory::parse`]
//! never fails and a typo costs one skipped line, not a crash or a
//! silently swallowed entry.
//!
//! # Where the file lives
//!
//! `$MODEM_DIRECTORY` if set, otherwise
//! `$XDG_CONFIG_HOME/modem/directory.tsv`, otherwise
//! `~/.config/modem/directory.tsv` (via `$HOME`). [`Directory::load`]
//! returns the resolved path alongside the parsed directory so the UI
//! can say where it looked when the file is not there - a missing file
//! is an empty directory, not an error: this module never creates or
//! writes it, only reads.
//!
//! # The format
//!
//! `name<TAB>number<TAB>note`, the note optional. `#` **as the very
//! first character of the line** marks a comment - not `#` anywhere in
//! the line, because a note is allowed to contain one (a hand-typed
//! "call after 6, ask for #2 on reception" is a normal note, not a
//! comment). Blank lines (empty or all whitespace) are skipped. Leading
//! and trailing whitespace on a field is trimmed, but a note may contain
//! interior spaces. A line with fewer than two tab-separated fields does
//! not have enough to be an entry: it is skipped **and counted** in
//! [`Directory::skipped`] - a hand-edited format that silently dropped a
//! mistyped line would hide the one mistake this format is most likely
//! to have.

use std::path::{Path, PathBuf};

/// One line of the directory: a name, a number to dial, and an optional
/// note. `note` is `String::new()` when the line had no third field, not
/// an `Option` - the UI treats an absent note and an empty one the same
/// way (nothing to show), so there is no second state worth
/// distinguishing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub number: String,
    pub note: String,
}

/// A parsed directory: the entries that parsed cleanly, plus a count of
/// the lines that did not. See this module's own doc for the format and
/// exactly what counts as malformed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Directory {
    entries: Vec<Entry>,
    skipped: usize,
}

impl Directory {
    /// Parses `text` into a [`Directory`]. Never fails - there is no
    /// error case a hand-typed file can hit that this returns `Err` for;
    /// a line that cannot be an entry is counted in [`Directory::skipped`]
    /// instead. See this module's own doc for the exact format rules.
    pub fn parse(text: &str) -> Directory {
        let mut entries = Vec::new();
        let mut skipped = 0usize;

        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            // Literally the first character of the raw line, not the
            // first non-whitespace character and not "anywhere in the
            // line" - see this module's own doc on why a note is allowed
            // to contain a '#'.
            if line.starts_with('#') {
                continue;
            }

            let mut fields = line.splitn(3, '\t');
            let name = fields.next();
            let number = fields.next();
            match (name, number) {
                (Some(name), Some(number)) => {
                    let note = fields.next().unwrap_or("");
                    entries.push(Entry {
                        name: name.trim().to_string(),
                        number: number.trim().to_string(),
                        note: note.trim().to_string(),
                    });
                }
                _ => skipped += 1,
            }
        }

        Directory { entries, skipped }
    }

    /// Reads and parses the real directory file - see this module's own
    /// doc for where it looks. A missing (or unreadable) file parses as
    /// empty text, i.e. an empty directory with nothing skipped, never an
    /// error - the caller only ever needs the resolved path, returned
    /// alongside, to tell the person where to create one.
    pub fn load() -> (Directory, PathBuf) {
        let path = directory_path();
        (load_from(&path), path)
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// How many lines did not parse as an entry - a comment or a blank
    /// line is not one of these, only a line that looked like it was
    /// trying to be an entry and came up short. See this module's own
    /// doc: counted, never silently dropped.
    pub fn skipped(&self) -> usize {
        self.skipped
    }
}

/// [`Directory::load`]'s file-reading half, factored out so a test can
/// exercise "a missing file parses as empty" against a real (deliberately
/// nonexistent) path without mutating process-global environment
/// variables to do it.
fn load_from(path: &Path) -> Directory {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    Directory::parse(&text)
}

/// Resolves the real directory path from the real environment - see this
/// module's own doc for the precedence. Thin wrapper around
/// [`resolve_path`] so the precedence logic itself stays testable without
/// touching real environment variables (which are process-global and
/// would otherwise race against every other test reading them).
fn directory_path() -> PathBuf {
    resolve_path(
        std::env::var("MODEM_DIRECTORY").ok(),
        std::env::var("XDG_CONFIG_HOME").ok(),
        std::env::var("HOME").ok(),
    )
}

/// The actual precedence: `$MODEM_DIRECTORY`, else
/// `$XDG_CONFIG_HOME/modem/directory.tsv`, else
/// `$HOME/.config/modem/directory.tsv`, else (no `$HOME` at all, which a
/// real login session always sets but a test harness might not) a plain
/// relative fallback - never a panic over an unset variable.
fn resolve_path(
    modem_directory: Option<String>,
    xdg_config_home: Option<String>,
    home: Option<String>,
) -> PathBuf {
    if let Some(p) = modem_directory {
        return PathBuf::from(p);
    }
    if let Some(xdg) = xdg_config_home {
        return PathBuf::from(xdg).join("modem").join("directory.tsv");
    }
    if let Some(home) = home {
        return PathBuf::from(home)
            .join(".config")
            .join("modem")
            .join("directory.tsv");
    }
    PathBuf::from(".config/modem/directory.tsv")
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Required test: a hand-written fixture covering the whole
    // format in one string, written as a literal (via `concat!`, which
    // is resolved at compile time from literal pieces - not generated by
    // any of the parsing code this test exercises). ---
    #[test]
    fn parses_a_hand_written_fixture_covering_every_case() {
        let text = concat!(
            "# a hand-edited phone book - keep it tab-separated\n",
            "\n",
            "Mum\t01234567890\tSunday calls\n",
            "Grandad\t01234000000\n",
            "  Aunt Jane  \t  01234111111  \t  ring back after 6, mention #golf  \n",
            "this line has no tabs at all, just prose\n",
        );

        let dir = Directory::parse(text);

        assert_eq!(
            dir.entries(),
            &[
                Entry {
                    name: "Mum".to_string(),
                    number: "01234567890".to_string(),
                    note: "Sunday calls".to_string(),
                },
                Entry {
                    name: "Grandad".to_string(),
                    number: "01234000000".to_string(),
                    note: String::new(),
                },
                Entry {
                    name: "Aunt Jane".to_string(),
                    number: "01234111111".to_string(),
                    note: "ring back after 6, mention #golf".to_string(),
                },
            ],
            "the comment, the blank line and the malformed line must not appear as entries, \
             and every field must be trimmed of its own leading/trailing whitespace only"
        );
        assert_eq!(
            dir.skipped(),
            1,
            "exactly one line (the one with no tabs at all) is malformed"
        );
    }

    // --- Mutation proof 3 (see the task report for the actual run): drop
    // malformed lines without counting them - change the `_ => skipped
    // += 1` arm to `_ => {}` and re-run the test above. It must fail on
    // the `skipped()` assertion.

    // --- Mutation proof 4 (see the task report for the actual run):
    // treat '#' anywhere in the line as a comment - change `line.
    // starts_with('#')` to `line.contains('#')` and re-run the test
    // above. The Aunt Jane line's note contains a '#' not in the first
    // column, so it must still parse as a real entry; under the mutation
    // it is dropped as a comment instead and the entries assertion fails.

    #[test]
    fn a_comment_must_start_at_the_very_first_character_not_after_leading_whitespace() {
        // Not required by the brief, but pins the literal "first column"
        // reading directly, separately from the fixture above: an
        // indented '#' is not a comment, so this line is malformed (no
        // tab) rather than silently ignored.
        let dir = Directory::parse("  # not a comment, just indented\n");
        assert!(dir.entries().is_empty());
        assert_eq!(dir.skipped(), 1);
    }

    #[test]
    fn a_line_with_only_a_name_and_no_tab_at_all_is_malformed_and_counted() {
        let dir = Directory::parse("just a name, no number\n");
        assert!(dir.entries().is_empty());
        assert_eq!(dir.skipped(), 1);
    }

    #[test]
    fn blank_lines_are_skipped_but_not_counted_as_malformed() {
        let dir = Directory::parse("\n   \n\t\n");
        assert!(dir.entries().is_empty());
        assert_eq!(
            dir.skipped(),
            0,
            "a blank line is not the same failure as a mistyped one"
        );
    }

    #[test]
    fn an_empty_file_is_an_empty_directory_with_nothing_skipped() {
        let dir = Directory::parse("");
        assert!(dir.entries().is_empty());
        assert_eq!(dir.skipped(), 0);
    }

    // --- Required test: a missing file is an empty directory, not an
    // error. Exercised against `load_from` with a path guaranteed not to
    // exist, rather than `load()` itself, so this never has to mutate
    // (and race on) the real process environment. ---
    #[test]
    fn a_missing_file_parses_as_an_empty_directory_not_an_error() {
        let dir = load_from(Path::new(
            "/definitely/does/not/exist/modem-directory-test-fixture.tsv",
        ));
        assert!(dir.entries().is_empty());
        assert_eq!(dir.skipped(), 0);
    }

    #[test]
    fn resolve_path_prefers_modem_directory_over_everything_else() {
        let p = resolve_path(
            Some("/custom/dir.tsv".to_string()),
            Some("/xdg".to_string()),
            Some("/home/dan".to_string()),
        );
        assert_eq!(p, PathBuf::from("/custom/dir.tsv"));
    }

    #[test]
    fn resolve_path_falls_back_to_xdg_config_home_when_modem_directory_is_unset() {
        let p = resolve_path(
            None,
            Some("/xdg".to_string()),
            Some("/home/dan".to_string()),
        );
        assert_eq!(p, PathBuf::from("/xdg/modem/directory.tsv"));
    }

    #[test]
    fn resolve_path_falls_back_to_home_dot_config_when_neither_is_set() {
        let p = resolve_path(None, None, Some("/home/dan".to_string()));
        assert_eq!(p, PathBuf::from("/home/dan/.config/modem/directory.tsv"));
    }
}
