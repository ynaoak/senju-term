//! Command-line handling for "open in external terminal".

/// Splits a command line into program + arguments without a shell.
///
/// Whitespace separates words; a double-quoted segment is one word (or part of
/// one) with the quotes removed, so `"C:\Program Files\x\x.exe" -a` yields two
/// words. There are no escapes: backslashes are literal, which is what Windows
/// paths need. An unterminated quote runs to the end of the string.
pub fn split_command_line(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut quoted = false;
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                in_word = true;
            }
            c if c.is_whitespace() && !quoted => {
                if in_word {
                    out.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            c => {
                cur.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_words() {
        assert_eq!(split_command_line("wt.exe -d  x"), ["wt.exe", "-d", "x"]);
    }

    #[test]
    fn quoted_path_with_spaces() {
        assert_eq!(
            split_command_line(r#""C:\Program Files\Alacritty\alacritty.exe" --working-directory {dir}"#),
            [r"C:\Program Files\Alacritty\alacritty.exe", "--working-directory", "{dir}"]
        );
    }

    #[test]
    fn dir_placeholder_stays_a_token_and_quotes_join() {
        assert_eq!(split_command_line("t -d {dir}"), ["t", "-d", "{dir}"]);
        assert_eq!(split_command_line(r#"t --x="a b" "" y"#), ["t", "--x=a b", "", "y"]);
    }

    #[test]
    fn empty_and_blank() {
        assert!(split_command_line("").is_empty());
        assert!(split_command_line("   \t ").is_empty());
    }
}
