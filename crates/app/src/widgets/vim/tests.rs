use super::*;

/// Keys as vim writes them: plain chars, plus `<esc>`, `<cr>`, `<bs>`,
/// `<del>`, arrows, `<home>`, `<end>`, `<c-z>` and `<c-r>`.
fn keys(text: &str) -> Vec<Key> {
    let mut keys = Vec::new();
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        let named = rest
            .strip_prefix('<')
            .and_then(|tail| tail.split_once('>'))
            .and_then(|(name, tail)| Some((named_key(name)?, tail)));
        if let Some((key, tail)) = named {
            keys.push(key);
            rest = tail;
            continue;
        }
        keys.push(Key::Char(c));
        rest = &rest[c.len_utf8()..];
    }
    keys
}

fn named_key(name: &str) -> Option<Key> {
    let key = match name {
        "esc" => Key::Escape,
        "cr" => Key::Enter,
        "bs" => Key::Backspace,
        "del" => Key::Delete,
        "left" => Key::Left,
        "right" => Key::Right,
        "up" => Key::Up,
        "down" => Key::Down,
        "home" => Key::Home,
        "end" => Key::End,
        "c-z" => Key::Undo,
        "c-r" => Key::Redo,
        _ => return None,
    };
    Some(key)
}

/// An editor with vim keys. In insert mode it types like the code editor
/// does, without the self-closing brackets.
struct Session {
    vim: Vim,
    text: String,
    cursor: usize,
    register: Register,
}

impl Session {
    /// `marked` is the text with `|` at the cursor.
    fn new(marked: &str, multiline: bool) -> Self {
        let cursor = marked.chars().position(|c| c == '|').expect("a | cursor");
        Self {
            vim: Vim::new(multiline),
            text: marked.replacen('|', "", 1),
            cursor,
            register: Register::default(),
        }
    }

    fn press(&mut self, typed: &str) -> &mut Self {
        for key in keys(typed) {
            let typing = self.vim.mode() == Mode::Insert
                && !matches!(key, Key::Escape | Key::Undo | Key::Redo);
            if typing {
                self.type_key(key);
                continue;
            }
            let mut edit = Edit {
                text: &mut self.text,
                cursor: self.cursor,
                register: &mut self.register,
            };
            self.vim.key(key, &mut edit);
            self.cursor = edit.cursor;
        }
        self
    }

    fn type_key(&mut self, key: Key) {
        let at = byte(&self.text, self.cursor);
        match key {
            Key::Char(c) => {
                self.text.insert(at, c);
                self.cursor += 1;
            }
            Key::Enter => {
                self.text.insert(at, '\n');
                self.cursor += 1;
            }
            Key::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                self.text.remove(byte(&self.text, self.cursor));
            }
            _ => {}
        }
    }

    /// The text with `|` at the cursor.
    fn marked(&self) -> String {
        let mut text = self.text.clone();
        text.insert(byte(&text, self.cursor), '|');
        text
    }

    /// The text with the visual selection in `[` `]`.
    fn selected(&self) -> String {
        let range = self
            .vim
            .selection(&self.text, self.cursor)
            .expect("a visual selection");
        let mut text = self.text.clone();
        text.insert(byte(&text, range.end), ']');
        text.insert(byte(&text, range.start), '[');
        text
    }
}

fn byte(text: &str, chars: usize) -> usize {
    text.char_indices()
        .nth(chars)
        .map_or(text.len(), |(i, _)| i)
}

/// Presses `typed` in a multiline editor from normal mode.
fn run(marked: &str, typed: &str) -> String {
    Session::new(marked, true).press(typed).marked()
}

/// Presses `typed` in a single-line editor from normal mode.
fn run_line(marked: &str, typed: &str) -> String {
    Session::new(marked, false).press(typed).marked()
}

fn selected(marked: &str, typed: &str) -> String {
    Session::new(marked, true).press(typed).selected()
}

fn register(marked: &str, typed: &str) -> Register {
    Session::new(marked, true).press(typed).register.clone()
}

fn chars(text: &str) -> Register {
    Register {
        text: text.to_owned(),
        linewise: false,
    }
}

fn lines(text: &str) -> Register {
    Register {
        text: text.to_owned(),
        linewise: true,
    }
}

// Motions ----------------------------------------------------------------

#[test]
fn h_moves_left() {
    assert_eq!(run("ab|c", "h"), "a|bc");
}

#[test]
fn h_stops_at_the_line_start() {
    assert_eq!(run("a\n|bc", "h"), "a\n|bc");
}

#[test]
fn l_stops_on_the_last_char() {
    assert_eq!(run("a|bc", "5l"), "ab|c");
}

#[test]
fn counts_repeat_motions() {
    assert_eq!(run("|abcdef", "3l"), "abc|def");
}

#[test]
fn j_keeps_the_column() {
    assert_eq!(run("ab|cd\nefgh", "j"), "abcd\nef|gh");
}

#[test]
fn j_through_a_short_line_comes_back_to_the_column() {
    assert_eq!(run("abc|d\nx\nabcd", "jj"), "abcd\nx\nabc|d");
}

#[test]
fn k_on_the_first_line_stays() {
    assert_eq!(run("a|b\ncd", "k"), "a|b\ncd");
}

#[test]
fn a_count_past_the_end_goes_to_the_last_line() {
    assert_eq!(run("|a\nb\nc", "9j"), "a\nb\n|c");
}

#[test]
fn dollar_sticks_to_line_ends() {
    assert_eq!(run("|ab\nabcd", "$j"), "ab\nabc|d");
}

#[test]
fn count_dollar_goes_down() {
    assert_eq!(run("|ab\nabc", "2$"), "ab\nab|c");
}

#[test]
fn w_moves_to_the_next_word() {
    assert_eq!(run("|foo bar", "w"), "foo |bar");
}

#[test]
fn w_stops_at_punctuation() {
    assert_eq!(run("|foo.bar", "w"), "foo|.bar");
}

#[test]
fn big_w_skips_punctuation() {
    assert_eq!(run("|foo.bar baz", "W"), "foo.bar |baz");
}

#[test]
fn w_crosses_lines() {
    assert_eq!(run("|foo\n  bar", "w"), "foo\n  |bar");
}

#[test]
fn w_stops_at_an_empty_line() {
    assert_eq!(run("|foo\n\nbar", "w"), "foo\n|\nbar");
}

#[test]
fn w_on_the_last_word_goes_to_the_end() {
    assert_eq!(run("foo |bar", "w"), "foo ba|r");
}

#[test]
fn w_treats_dollar_as_punctuation() {
    assert_eq!(run("{ |$in: [] }", "w"), "{ $|in: [] }");
}

#[test]
fn b_moves_to_the_word_start() {
    assert_eq!(run("foo ba|r", "b"), "foo |bar");
}

#[test]
fn b_from_a_word_start_goes_to_the_previous_word() {
    assert_eq!(run("foo |bar", "b"), "|foo bar");
}

#[test]
fn b_crosses_lines() {
    assert_eq!(run("foo\n|bar", "b"), "|foo\nbar");
}

#[test]
fn b_stops_at_an_empty_line() {
    assert_eq!(run("a\n\n|b", "b"), "a\n|\nb");
}

#[test]
fn big_b_skips_punctuation() {
    assert_eq!(run("a foo.ba|r", "B"), "a |foo.bar");
}

#[test]
fn e_moves_to_the_word_end() {
    assert_eq!(run("|foo bar", "e"), "fo|o bar");
}

#[test]
fn e_at_a_word_end_goes_to_the_next_one() {
    assert_eq!(run("|foo bar", "ee"), "foo ba|r");
}

#[test]
fn big_e_skips_punctuation() {
    assert_eq!(run("|foo.bar baz", "E"), "foo.ba|r baz");
}

#[test]
fn zero_goes_to_the_line_start() {
    assert_eq!(run("  ab|c", "0"), "|  abc");
}

#[test]
fn caret_goes_to_the_first_non_blank() {
    assert_eq!(run("  ab|c", "^"), "  |abc");
}

#[test]
fn end_and_home_keys() {
    assert_eq!(run("  a|bc", "<end>"), "  ab|c");
    assert_eq!(run("  a|bc", "<home>"), "|  abc");
}

#[test]
fn gg_goes_to_the_first_line() {
    assert_eq!(run("  a\nb\n|c", "gg"), "  |a\nb\nc");
}

#[test]
fn big_g_goes_to_the_last_line() {
    assert_eq!(run("|a\nb\n  c", "G"), "a\nb\n  |c");
}

#[test]
fn a_count_before_g_names_the_line() {
    assert_eq!(run("|a\nb\nc", "2G"), "a\n|b\nc");
    assert_eq!(run("a\nb\n|c", "2gg"), "a\n|b\nc");
}

#[test]
fn enter_goes_to_the_next_lines_first_non_blank() {
    assert_eq!(run("|a\n  b", "<cr>"), "a\n  |b");
}

#[test]
fn arrows_move_like_hjkl() {
    assert_eq!(run("|ab\ncd", "<right><down>"), "ab\nc|d");
    assert_eq!(run("ab\nc|d", "<up><left>"), "|ab\ncd");
}

#[test]
fn backspace_moves_left() {
    assert_eq!(run("ab|c", "<bs>"), "a|bc");
}

#[test]
fn f_finds_a_char() {
    assert_eq!(run("|a,b,c", "f,"), "a|,b,c");
}

#[test]
fn a_count_finds_a_later_char() {
    assert_eq!(run("|a,b,c", "2f,"), "a,b|,c");
}

#[test]
fn f_without_a_match_stays() {
    assert_eq!(run("|abc", "fz"), "|abc");
}

#[test]
fn f_stays_on_its_line() {
    assert_eq!(run("|ab\nc,", "f,"), "|ab\nc,");
}

#[test]
fn t_stops_before_the_char() {
    assert_eq!(run("|ab,c", "t,"), "a|b,c");
}

#[test]
fn big_f_finds_backwards() {
    assert_eq!(run("a,b|c", "F,"), "a|,bc");
}

#[test]
fn big_t_stops_after_the_char() {
    assert_eq!(run("a,b|c", "T,"), "a,|bc");
}

#[test]
fn semicolon_repeats_a_find() {
    assert_eq!(run("|a,b,c,d", "f,;"), "a,b|,c,d");
}

#[test]
fn comma_repeats_a_find_backwards() {
    assert_eq!(run("|a,b,c,d", "f,;;,"), "a,b|,c,d");
}

#[test]
fn semicolon_after_t_gets_past_the_char() {
    assert_eq!(run("|a,b,c", "t,;"), "a,|b,c");
}

#[test]
fn percent_jumps_between_brackets() {
    assert_eq!(run("|{ a: [1] }", "%"), "{ a: [1] |}");
    assert_eq!(run("{ a: [1] |}", "%"), "|{ a: [1] }");
}

#[test]
fn percent_uses_the_next_bracket_on_the_line() {
    assert_eq!(run("|a: [1, 2]", "%"), "a: [1, 2|]");
}

#[test]
fn percent_crosses_lines() {
    assert_eq!(run("|{\n  a\n}", "%"), "{\n  a\n|}");
}

#[test]
fn percent_without_a_bracket_stays() {
    assert_eq!(run("|ab", "%"), "|ab");
}

// Operators --------------------------------------------------------------

#[test]
fn dw_deletes_a_word_and_its_blank() {
    assert_eq!(run("|foo bar", "dw"), "|bar");
}

#[test]
fn counts_before_and_after_the_operator_both_work() {
    assert_eq!(run("|a b c", "d2w"), "|c");
    assert_eq!(run("|a b c", "2dw"), "|c");
}

#[test]
fn counts_before_and_after_the_operator_multiply() {
    assert_eq!(run("|a b c d e", "2d2w"), "|e");
}

#[test]
fn dw_on_a_lines_last_word_keeps_the_newline() {
    assert_eq!(run("foo |bar\n  baz", "dw"), "foo| \n  baz");
}

#[test]
fn dw_on_trailing_blanks_deletes_to_the_line_end() {
    assert_eq!(run("foo|  \nbar", "dw"), "fo|o\nbar");
}

#[test]
fn d2w_crosses_a_line() {
    assert_eq!(run("a |b\nc d", "d2w"), "a |d");
}

#[test]
fn de_keeps_the_blank() {
    assert_eq!(run("|foo bar", "de"), "| bar");
}

#[test]
fn db_deletes_backwards() {
    assert_eq!(run("foo ba|r", "db"), "foo |r");
}

#[test]
fn big_d_deletes_to_the_line_end() {
    assert_eq!(run("a|bc\nd", "D"), "|a\nd");
    assert_eq!(run("a|bc\nd", "d$"), "|a\nd");
}

#[test]
fn d0_deletes_to_the_line_start() {
    assert_eq!(run("ab|c", "d0"), "|c");
}

#[test]
fn d_caret_deletes_to_the_first_non_blank() {
    assert_eq!(run("  ab|c", "d^"), "  |c");
}

#[test]
fn dd_deletes_the_line() {
    assert_eq!(run("a\n|b\nc", "dd"), "a\n|c");
}

#[test]
fn dd_on_the_last_line_takes_the_newline_before_it() {
    assert_eq!(run("a\n|b", "dd"), "|a");
}

#[test]
fn dd_on_the_only_line_empties_the_text() {
    assert_eq!(run("|abc", "dd"), "|");
}

#[test]
fn count_dd_deletes_lines() {
    assert_eq!(run("|a\nb\nc", "2dd"), "|c");
}

#[test]
fn count_dd_past_the_end_deletes_to_the_end() {
    assert_eq!(run("a\n|b\nc", "9dd"), "|a");
}

#[test]
fn dd_lands_on_the_first_non_blank() {
    assert_eq!(run("|a\n  b", "dd"), "  |b");
}

#[test]
fn dj_deletes_two_lines() {
    assert_eq!(run("|a\nb\nc", "dj"), "|c");
}

#[test]
fn dk_deletes_two_lines() {
    assert_eq!(run("a\nb\n|c", "dk"), "|a");
}

#[test]
fn dk_on_the_first_line_does_nothing() {
    assert_eq!(run("|a\nb", "dk"), "|a\nb");
}

#[test]
fn dgg_deletes_to_the_first_line() {
    assert_eq!(run("a\n|b\nc", "dgg"), "|c");
}

#[test]
fn d_big_g_deletes_to_the_last_line() {
    assert_eq!(run("a\n|b\nc", "dG"), "|a");
}

#[test]
fn df_deletes_through_the_char() {
    assert_eq!(run("|a,b,c", "df,"), "|b,c");
}

#[test]
fn dt_deletes_up_to_the_char() {
    assert_eq!(run("|foo(bar)", "dt("), "|(bar)");
}

#[test]
fn d_big_f_deletes_back_to_the_char() {
    assert_eq!(run("a,bc|d", "dF,"), "a|d");
}

#[test]
fn d_percent_deletes_the_brackets() {
    assert_eq!(run("x |(a (b)) y", "d%"), "x | y");
}

#[test]
fn d_semicolon_repeats_the_find() {
    assert_eq!(run("|a,b,c", "f,d;"), "a|c");
}

#[test]
fn x_deletes_the_char() {
    assert_eq!(run("a|bc", "x"), "a|c");
}

#[test]
fn count_x_stops_at_the_line_end() {
    assert_eq!(run("a|bc\nd", "5x"), "|a\nd");
}

#[test]
fn x_on_an_empty_line_does_nothing() {
    assert_eq!(run("a\n|\nb", "x"), "a\n|\nb");
}

#[test]
fn big_x_deletes_before_the_cursor() {
    assert_eq!(run("ab|c", "X"), "a|c");
}

#[test]
fn big_x_at_the_line_start_does_nothing() {
    assert_eq!(run("a\n|b", "X"), "a\n|b");
}

#[test]
fn delete_key_works_like_x() {
    assert_eq!(run("a|bc", "<del>"), "a|c");
}

#[test]
fn cw_changes_to_the_word_end() {
    assert_eq!(run("|foo bar", "cwbaz<esc>"), "ba|z bar");
}

#[test]
fn cw_on_a_words_last_char_changes_only_it() {
    assert_eq!(run("fo|o bar", "cwx<esc>"), "fo|x bar");
}

#[test]
fn c2w_changes_two_words() {
    assert_eq!(run("|a b c", "c2wx<esc>"), "|x c");
}

#[test]
fn cw_on_blanks_changes_the_blanks() {
    assert_eq!(run("a|  b", "cw-<esc>"), "a|-b");
}

#[test]
fn cc_keeps_the_indent() {
    assert_eq!(run("  a|b\nc", "ccx<esc>"), "  |x\nc");
}

#[test]
fn big_c_changes_to_the_line_end() {
    assert_eq!(run("a|bc", "Cx<esc>"), "a|x");
}

#[test]
fn s_changes_the_char() {
    assert_eq!(run("a|bc", "sx<esc>"), "a|xc");
}

#[test]
fn big_s_changes_the_line() {
    assert_eq!(run("  a|b", "Sx<esc>"), "  |x");
}

#[test]
fn cj_changes_two_lines() {
    assert_eq!(run("|a\nb\nc", "cjx<esc>"), "|x\nc");
}

#[test]
fn yw_then_big_p_puts_the_word_before() {
    assert_eq!(run("|foo bar", "ywP"), "foo| foo bar");
}

#[test]
fn y_leaves_the_text() {
    assert_eq!(run("|foo", "yw"), "|foo");
}

#[test]
fn y_fills_the_register() {
    assert_eq!(register("|foo bar", "yw"), chars("foo "));
}

#[test]
fn yy_fills_the_register_with_a_line() {
    assert_eq!(register("a\n|b\nc", "yy"), lines("b\n"));
}

#[test]
fn d_fills_the_register() {
    assert_eq!(register("|foo bar", "de"), chars("foo"));
}

#[test]
fn x_fills_the_register() {
    assert_eq!(register("a|bc", "2x"), chars("bc"));
}

#[test]
fn c_fills_the_register() {
    assert_eq!(register("|foo bar", "cwx<esc>"), chars("foo"));
}

#[test]
fn big_y_yanks_to_the_line_end() {
    assert_eq!(register("a|bc", "Y"), chars("bc"));
}

#[test]
fn y_backwards_moves_the_cursor_to_the_start() {
    assert_eq!(run("foo ba|r", "yb"), "foo |bar");
}

#[test]
fn yk_moves_the_cursor_up() {
    assert_eq!(run("ab\nc|d", "yk"), "|ab\ncd");
}

#[test]
fn yj_keeps_the_cursor() {
    assert_eq!(run("a|b\ncd", "yj"), "a|b\ncd");
}

#[test]
fn yi_quote_yanks_the_string() {
    assert_eq!(register("{ a: 'f|oo' }", "yi'"), chars("foo"));
}

// Put --------------------------------------------------------------------

#[test]
fn p_puts_after_the_cursor() {
    assert_eq!(run("|ab", "ylp"), "a|ab");
}

#[test]
fn count_p_puts_several_times() {
    assert_eq!(run("|ab", "yl3p"), "aaa|ab");
}

#[test]
fn p_on_an_empty_line_puts_at_the_cursor() {
    assert_eq!(run("|a\n\nb", "yljp"), "a\n|a\nb");
}

#[test]
fn yy_p_puts_a_line_below() {
    assert_eq!(run("|a\nb", "yyp"), "a\n|a\nb");
}

#[test]
fn yy_big_p_puts_a_line_above() {
    assert_eq!(run("a\n|b", "yyP"), "a\n|b\nb");
}

#[test]
fn yy_p_on_the_last_line() {
    assert_eq!(run("a\n|b", "yyp"), "a\nb\n|b");
}

#[test]
fn linewise_p_lands_on_the_first_non_blank() {
    assert_eq!(run("|  a\nb", "yyjp"), "  a\nb\n  |a");
}

#[test]
fn count_p_puts_lines_several_times() {
    assert_eq!(run("|a", "yy2p"), "a\n|a\na");
}

#[test]
fn ddp_swaps_lines() {
    assert_eq!(run("|a\nb\nc", "ddp"), "b\n|a\nc");
}

#[test]
fn xp_swaps_chars() {
    assert_eq!(run("|ab", "xp"), "b|a");
}

#[test]
fn p_of_text_with_a_newline_puts_the_cursor_at_its_start() {
    assert_eq!(run("|ab\ncd", "vjy$p"), "ab|ab\nc\ncd");
}

#[test]
fn p_with_an_empty_register_does_nothing() {
    assert_eq!(run("a|b", "p"), "a|b");
}

#[test]
fn the_register_is_shared_between_editors() {
    let mut shell = Session::new("|db.users", true);
    shell.press("yiw");
    let mut filter = Session::new("{ |}", false);
    filter.register = shell.register.clone();
    filter.press("P");
    assert_eq!(filter.marked(), "{ d|b}");
}

// Text objects -----------------------------------------------------------

#[test]
fn diw_deletes_the_word() {
    assert_eq!(run("foo b|ar baz", "diw"), "foo | baz");
}

#[test]
fn diw_on_blanks_deletes_the_blanks() {
    assert_eq!(run("a | b", "diw"), "a|b");
}

#[test]
fn diw_on_punctuation_deletes_the_run() {
    assert_eq!(run("a.|.b", "diw"), "a|b");
}

#[test]
fn count_diw_takes_more_runs() {
    assert_eq!(run("|a b c", "d3iw"), "| c");
}

#[test]
fn diw_on_an_empty_line_does_nothing() {
    assert_eq!(run("a\n|\nb", "diw"), "a\n|\nb");
}

#[test]
fn daw_takes_the_trailing_blank() {
    assert_eq!(run("foo b|ar baz", "daw"), "foo |baz");
}

#[test]
fn daw_at_the_line_end_takes_the_leading_blank() {
    assert_eq!(run("foo b|ar", "daw"), "fo|o");
}

#[test]
fn daw_on_blanks_takes_the_next_word() {
    assert_eq!(run("a | b c", "daw"), "a| c");
}

#[test]
fn ciw_changes_the_word() {
    assert_eq!(run("foo b|ar", "ciwx<esc>"), "foo |x");
}

#[test]
fn di_big_w_takes_punctuation() {
    assert_eq!(run("x fo|o.bar y", "diW"), "x | y");
}

#[test]
fn da_big_w_takes_the_blank() {
    assert_eq!(run("x fo|o.bar y", "daW"), "x |y");
}

#[test]
fn di_quote_deletes_inside_the_quotes() {
    assert_eq!(run("{ a: 'f|oo' }", "di'"), "{ a: '|' }");
}

#[test]
fn da_quote_takes_the_quotes_and_the_trailing_blank() {
    assert_eq!(run("{ a: 'f|oo' }", "da'"), "{ a: |}");
}

#[test]
fn da_quote_at_the_line_end_takes_the_leading_blank() {
    assert_eq!(run("a: 'f|oo'", "da'"), "a|:");
}

#[test]
fn di_quote_before_a_string_uses_the_next_one() {
    assert_eq!(run("|a: \"b\"", "di\""), "a: \"|\"");
}

#[test]
fn di_quote_on_the_closing_quote() {
    assert_eq!(run("'ab|'", "di'"), "'|'");
}

#[test]
fn di_quote_between_strings_uses_the_next_one() {
    assert_eq!(run("'a' |, 'b'", "di'"), "'a' , '|'");
}

#[test]
fn di_quote_skips_escaped_quotes() {
    assert_eq!(run("'a\\'|b'", "di'"), "'|'");
}

#[test]
fn di_quote_without_a_string_does_nothing() {
    assert_eq!(run("a|b", "di'"), "a|b");
}

#[test]
fn di_backtick() {
    assert_eq!(run("`a|b`", "di`"), "`|`");
}

#[test]
fn ci_quote_changes_the_string() {
    assert_eq!(run("{ s: 'a|b' }", "ci'xy<esc>"), "{ s: 'x|y' }");
}

#[test]
fn di_paren_deletes_inside() {
    assert_eq!(run("f(a, |b)", "di("), "f(|)");
}

#[test]
fn da_paren_takes_the_parens() {
    assert_eq!(run("f(a, |b) x", "da("), "f| x");
}

#[test]
fn dib_is_di_paren() {
    assert_eq!(run("(a|b)", "dib"), "(|)");
}

#[test]
fn di_paren_on_either_paren() {
    assert_eq!(run("|(ab)", "di("), "(|)");
    assert_eq!(run("(ab|)", "di)"), "(|)");
}

#[test]
fn di_curly_picks_the_innermost_block() {
    assert_eq!(run("{ a: { b|: 1 } }", "di{"), "{ a: {|} }");
}

#[test]
fn a_count_picks_an_outer_block() {
    assert_eq!(run("{ a: { b|: 1 } }", "2di{"), "{|}");
}

#[test]
fn di_big_b_is_di_curly() {
    assert_eq!(run("{ a|: 1 }", "diB"), "{|}");
}

#[test]
fn di_curly_over_lines_keeps_the_braces_lines() {
    assert_eq!(run("{\n  a|: 1\n}", "di{"), "{\n|}");
}

#[test]
fn ci_curly_over_lines_leaves_an_indented_line() {
    assert_eq!(run("{\n  a|: 1\n}", "ci{x<esc>"), "{\n  |x\n}");
}

#[test]
fn di_curly_with_text_after_the_opener_is_charwise() {
    assert_eq!(run("{ a,\n  b|: 1 }", "di{"), "{|}");
}

#[test]
fn di_square() {
    assert_eq!(run("[1, |2]", "di["), "[|]");
}

#[test]
fn di_paren_without_parens_does_nothing() {
    assert_eq!(run("a|b", "di("), "a|b");
}

#[test]
fn a_count_skips_sibling_blocks() {
    assert_eq!(run("(a)(|b)", "2di("), "(a)(|b)");
}

#[test]
fn di_paren_across_other_brackets() {
    assert_eq!(run("f([a], {|b})", "di("), "f(|)");
}

#[test]
fn yi_curly_yanks_lines_linewise() {
    assert_eq!(register("{\n  a|: 1\n}", "yi{"), lines("  a: 1\n"));
}

// Insert -----------------------------------------------------------------

#[test]
fn i_inserts_before_the_cursor() {
    assert_eq!(run("a|b", "ix<esc>"), "a|xb");
}

#[test]
fn a_appends_after_the_cursor() {
    assert_eq!(run("a|b", "ax<esc>"), "ab|x");
}

#[test]
fn a_on_an_empty_text() {
    assert_eq!(run("|", "ax<esc>"), "|x");
}

#[test]
fn big_i_inserts_at_the_first_non_blank() {
    assert_eq!(run("  a|b", "Ix<esc>"), "  |xab");
}

#[test]
fn big_a_appends_at_the_line_end() {
    assert_eq!(run("a|b\nc", "Ax<esc>"), "ab|x\nc");
}

#[test]
fn o_opens_a_line_below_with_the_indent() {
    assert_eq!(run("  a|b\nc", "ox<esc>"), "  ab\n  |x\nc");
}

#[test]
fn big_o_opens_a_line_above() {
    assert_eq!(run("  a|b", "Ox<esc>"), "  |x\n  ab");
}

#[test]
fn escape_moves_left() {
    assert_eq!(run("ab|c", "i<esc>"), "a|bc");
}

#[test]
fn escape_at_the_line_start_stays() {
    assert_eq!(run("a\n|b", "i<esc>"), "a\n|b");
}

#[test]
fn insert_and_escape_switch_modes() {
    let mut session = Session::new("|a", true);
    session.press("i");
    assert_eq!(session.vim.mode(), Mode::Insert);
    session.press("<esc>");
    assert_eq!(session.vim.mode(), Mode::Normal);
}

#[test]
fn typing_in_insert_mode_reaches_the_editor() {
    assert_eq!(run("|", "idd<esc>"), "d|d");
}

// Small edits ------------------------------------------------------------

#[test]
fn r_replaces_the_char() {
    assert_eq!(run("a|bc", "rx"), "a|xc");
}

#[test]
fn count_r_replaces_several_chars() {
    assert_eq!(run("|abc", "2rx"), "x|xc");
}

#[test]
fn r_past_the_line_end_does_nothing() {
    assert_eq!(run("a|b", "3rx"), "a|b");
}

#[test]
fn r_then_escape_cancels() {
    assert_eq!(run("a|b", "r<esc>"), "a|b");
}

#[test]
fn big_j_joins_with_one_space() {
    assert_eq!(run("|a\n  b", "J"), "a| b");
}

#[test]
fn big_j_adds_no_space_before_a_paren() {
    assert_eq!(run("|f(a\n)", "J"), "f(a|)");
}

#[test]
fn big_j_adds_no_space_after_a_blank() {
    assert_eq!(run("|a \nb", "J"), "a |b");
}

#[test]
fn count_big_j_joins_more_lines() {
    assert_eq!(run("|a\nb\nc", "3J"), "a b| c");
}

#[test]
fn big_j_on_the_last_line_does_nothing() {
    assert_eq!(run("a\n|b", "J"), "a\n|b");
}

#[test]
fn big_j_with_an_empty_line() {
    assert_eq!(run("|a\n\nb", "J"), "|a\nb");
}

#[test]
fn tilde_toggles_case_and_moves() {
    assert_eq!(run("|aB", "~"), "A|B");
}

#[test]
fn count_tilde_stops_at_the_line_end() {
    assert_eq!(run("|aBc\nd", "5~"), "Ab|C\nd");
}

// Visual -----------------------------------------------------------------

#[test]
fn v_selects_chars() {
    assert_eq!(selected("a|bcd", "vl"), "a[bc]d");
}

#[test]
fn v_selects_backwards() {
    assert_eq!(selected("ab|cd", "vh"), "a[bc]d");
}

#[test]
fn big_v_selects_lines() {
    assert_eq!(selected("a\nb|c\nd", "V"), "a\n[bc\n]d");
}

#[test]
fn big_v_with_j_selects_two_lines() {
    assert_eq!(selected("|a\nb\nc", "Vj"), "[a\nb\n]c");
}

#[test]
fn v_with_a_count() {
    assert_eq!(selected("|abcdef", "v3l"), "[abcd]ef");
}

#[test]
fn v_with_a_word_motion() {
    assert_eq!(selected("|foo bar", "ve"), "[foo] bar");
}

#[test]
fn vd_deletes_the_selection() {
    assert_eq!(run("a|bcd", "vld"), "a|d");
}

#[test]
fn vx_deletes_the_selection() {
    assert_eq!(run("a|bc", "vx"), "a|c");
}

#[test]
fn big_vd_deletes_lines() {
    assert_eq!(run("a\n|b\nc", "Vd"), "a\n|c");
}

#[test]
fn vj_d_deletes_across_lines() {
    assert_eq!(run("a|b\ncd", "vjd"), "|a");
}

#[test]
fn vy_yanks_and_goes_to_the_start() {
    assert_eq!(run("ab|cd", "vhy"), "a|bcd");
}

#[test]
fn vy_fills_the_register() {
    assert_eq!(register("ab|cd", "vhy"), chars("bc"));
}

#[test]
fn big_vy_fills_the_register_with_lines() {
    assert_eq!(register("|a\nb", "Vjy"), lines("a\nb\n"));
}

#[test]
fn big_vy_then_p_puts_lines() {
    assert_eq!(run("|a\nb", "Vyjp"), "a\nb\n|a");
}

#[test]
fn vc_changes_the_selection() {
    assert_eq!(run("a|bcd", "vlcx<esc>"), "a|xd");
}

#[test]
fn o_swaps_the_selection_ends() {
    assert_eq!(selected("ab|cd", "vloh"), "a[bcd]");
}

#[test]
fn viw_selects_the_word() {
    assert_eq!(selected("foo b|ar", "viw"), "foo [bar]");
}

#[test]
fn vi_curly_selects_inside() {
    assert_eq!(selected("{ a|: 1 }", "vi{"), "{[ a: 1 ]}");
}

#[test]
fn va_quote_selects_the_string() {
    assert_eq!(selected("x 'a|b' y", "va'"), "x ['ab' ]y");
}

#[test]
fn vp_replaces_the_selection() {
    assert_eq!(run("|ab cd", "yiwwviwp"), "ab a|b");
}

#[test]
fn vp_puts_the_replaced_text_in_the_register() {
    assert_eq!(register("|ab cd", "yiwwviwp"), chars("cd"));
}

#[test]
fn v_big_p_keeps_the_register() {
    assert_eq!(register("|ab cd", "yiwwviwP"), chars("ab"));
}

#[test]
fn vp_of_lines_into_a_char_selection_splits_the_line() {
    assert_eq!(run("|x\nabcd", "yyjlvlp"), "x\na\n|x\nd");
}

#[test]
fn big_vp_replaces_lines() {
    assert_eq!(run("|a\nb\nc", "yyjVp"), "a\n|a\nc");
}

#[test]
fn escape_leaves_visual_mode() {
    let mut session = Session::new("a|b", true);
    session.press("v<esc>");
    assert_eq!(session.vim.mode(), Mode::Normal);
    assert_eq!(session.vim.selection(&session.text, session.cursor), None);
}

#[test]
fn v_again_leaves_visual_mode() {
    let mut session = Session::new("a|b", true);
    session.press("vv");
    assert_eq!(session.vim.mode(), Mode::Normal);
}

#[test]
fn big_v_in_visual_switches_to_lines() {
    let mut session = Session::new("a|b\nc", true);
    session.press("vV");
    assert_eq!(session.vim.mode(), Mode::VisualLine);
    assert_eq!(session.selected(), "[ab\n]c");
}

#[test]
fn select_starts_visual_mode_from_the_mouse() {
    let mut session = Session::new("ab|cd", true);
    session.vim.select(0);
    assert_eq!(session.vim.mode(), Mode::Visual);
    assert_eq!(session.selected(), "[abc]d");
}

// Undo -------------------------------------------------------------------

#[test]
fn u_undoes_a_delete() {
    assert_eq!(run("|a b", "dwu"), "|a b");
}

#[test]
fn ctrl_r_redoes() {
    assert_eq!(run("|a b", "dwu<c-r>"), "|b");
}

#[test]
fn an_insert_session_undoes_at_once() {
    assert_eq!(run("|", "iabc<esc>u"), "|");
}

#[test]
fn a_change_and_its_typing_undo_at_once() {
    assert_eq!(run("|foo bar", "cwx<esc>u"), "|foo bar");
}

#[test]
fn count_u_undoes_several_steps() {
    assert_eq!(run("|abc", "xxx2u"), "|bc");
}

#[test]
fn a_new_change_clears_redo() {
    assert_eq!(run("|abc", "xux<c-r>"), "|bc");
}

#[test]
fn u_with_nothing_to_undo_does_nothing() {
    assert_eq!(run("a|b", "u"), "a|b");
}

#[test]
fn moving_is_not_an_undo_step() {
    assert_eq!(run("|a b", "xwu"), "|a b");
}

#[test]
fn ctrl_z_in_normal_mode_undoes() {
    assert_eq!(run("|ab", "x<c-z>"), "|ab");
}

#[test]
fn ctrl_z_in_insert_mode_undoes_the_session_so_far() {
    let mut session = Session::new("|", true);
    session.press("iab<c-z>");
    assert_eq!(session.marked(), "|");
    assert_eq!(session.vim.mode(), Mode::Insert);
}

#[test]
fn an_insert_started_on_focus_undoes_at_once() {
    let mut session = Session::new("a|", true);
    session.vim.start_insert(&session.text, session.cursor);
    session.press("bc<esc>u");
    assert_eq!(session.text, "a");
}

#[test]
fn an_escape_without_typing_is_no_undo_step() {
    assert_eq!(run("|ab", "xi<esc>u"), "|ab");
}

// Pending keys and counts ------------------------------------------------

#[test]
fn escape_cancels_a_pending_operator() {
    assert_eq!(run("|a b", "d<esc>w"), "a |b");
}

#[test]
fn an_invalid_key_cancels_a_pending_operator() {
    assert_eq!(run("|a b", "dzw"), "a |b");
}

#[test]
fn unknown_keys_do_nothing() {
    assert_eq!(run("a|b", "Q"), "a|b");
}

#[test]
fn counts_are_capped() {
    assert_eq!(run("|ab", "99999999x"), "|");
}

#[test]
fn a_zero_inside_a_count_is_a_digit() {
    assert_eq!(run("|abcdefghijkl", "10l"), "abcdefghij|kl");
}

#[test]
fn editors_start_in_normal_mode() {
    assert_eq!(Vim::new(true).mode(), Mode::Normal);
}

// Single line ------------------------------------------------------------

#[test]
fn single_line_o_does_nothing() {
    let mut session = Session::new("a|b", false);
    session.press("o");
    assert_eq!(session.vim.mode(), Mode::Normal);
    assert_eq!(session.marked(), "a|b");
}

#[test]
fn single_line_dd_empties_the_text() {
    assert_eq!(run_line("a|b", "dd"), "|");
}

#[test]
fn single_line_puts_lines_inline() {
    assert_eq!(run_line("|ab", "yyp"), "aa|bb");
}

#[test]
fn single_line_j_and_big_j_do_nothing() {
    assert_eq!(run_line("a|b", "jJ"), "a|b");
}

#[test]
fn single_line_puts_text_with_newlines_on_one_line() {
    let mut session = Session::new("|", false);
    session.register = lines("a\nb\n");
    session.press("p");
    assert_eq!(session.text, "a b");
}

// Other text -------------------------------------------------------------

#[test]
fn works_on_chars_not_bytes() {
    assert_eq!(run("|åäö x", "dw"), "|x");
    assert_eq!(run("é|ü", "x"), "|é");
}

#[test]
fn clamp_keeps_the_cursor_off_the_newline() {
    assert_eq!(Vim::new(true).clamp("ab\ncd", 2), 1);
}

#[test]
fn clamp_allows_an_empty_line() {
    assert_eq!(Vim::new(true).clamp("ab\n\ncd", 3), 3);
}

#[test]
fn a_cursor_past_the_end_is_brought_back() {
    assert_eq!(run("ab|", "x"), "|a");
}
