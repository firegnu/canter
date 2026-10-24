// Accept only complete, independently readable scripts. In particular, do not
// load scripts from files or try to infer the safety of unfamiliar sed syntax.
pub(super) fn is_readonly(args: &[String], path_allowed: &impl Fn(&str) -> bool) -> bool {
    if !args.iter().all(|arg| path_allowed(arg)) {
        return false;
    }
    let mut scripts = Vec::new();
    let mut options = true;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if options && arg == "--" {
            options = false;
            continue;
        }
        if options && arg.starts_with('-') {
            match arg.as_str() {
                "--quiet" | "--silent" | "--regexp-extended" => {}
                "-e" | "--expression" => {
                    let Some(script) = args.next() else {
                        return false;
                    };
                    scripts.push(script.as_str());
                }
                _ if arg.starts_with("--expression=") => scripts.push(&arg[13..]),
                _ if !arg.starts_with("--") => {
                    for (index, flag) in arg[1..].char_indices() {
                        match flag {
                            'n' | 'E' | 'r' => {}
                            'e' => {
                                let script = &arg[index + 2..];
                                if script.is_empty() {
                                    let Some(script) = args.next() else {
                                        return false;
                                    };
                                    scripts.push(script.as_str());
                                } else {
                                    scripts.push(script);
                                }
                                break;
                            }
                            _ => return false,
                        }
                    }
                }
                _ => return false,
            }
        } else {
            if scripts.is_empty() {
                scripts.push(arg);
            }
        }
    }
    // Options remain checked after script/file operands too (GNU option permutation).
    !scripts.is_empty() && scripts.iter().all(|script| safe_script(script))
}

fn safe_script(script: &str) -> bool {
    let mut rest = script.as_bytes();
    while !rest.is_empty() {
        rest = trim(rest);
        if rest.is_empty() {
            return true;
        }
        if !address(&mut rest) {
            return false;
        }
        if rest.first() == Some(&b',') {
            rest = &rest[1..];
            if !address(&mut rest) {
                return false;
            }
        }
        rest = trim(rest);
        if rest.first() == Some(&b'!') {
            rest = trim(&rest[1..]);
        }
        let Some((&command, tail)) = rest.split_first() else {
            return false;
        };
        rest = tail;
        match command {
            b's' => {
                let Some((&delimiter, tail)) = rest.split_first() else {
                    return false;
                };
                if !b"/|#:@".contains(&delimiter) {
                    return false;
                }
                rest = tail;
                if !delimited(&mut rest, delimiter) || !delimited(&mut rest, delimiter) {
                    return false;
                }
                while rest
                    .first()
                    .is_some_and(|flag| b"gpI0123456789".contains(flag))
                {
                    rest = &rest[1..];
                }
            }
            b'p' | b'P' | b'd' | b'D' | b'n' | b'N' | b'h' | b'H' | b'g' | b'G' | b'x' | b'=' => {}
            b'q' => {
                rest = trim(rest);
                while rest.first().is_some_and(u8::is_ascii_digit) {
                    rest = &rest[1..];
                }
            }
            _ => return false, // Includes w/W/e/r/R and text, branch or block commands.
        }
        rest = trim(rest);
        if rest.is_empty() {
            return true;
        }
        if rest[0] != b';' {
            return false;
        }
        rest = &rest[1..];
    }
    true
}

fn trim(mut text: &[u8]) -> &[u8] {
    while text
        .first()
        .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
    {
        text = &text[1..];
    }
    text
}

fn address(rest: &mut &[u8]) -> bool {
    *rest = trim(rest);
    if rest.first() == Some(&b'/') {
        *rest = &rest[1..];
        return delimited(rest, b'/');
    }
    while rest.first().is_some_and(u8::is_ascii_digit) {
        *rest = &rest[1..];
    }
    true
}

fn delimited(rest: &mut &[u8], delimiter: u8) -> bool {
    while let Some((&ch, tail)) = rest.split_first() {
        *rest = tail;
        if ch == delimiter {
            return true;
        }
        // Bracket expressions and embedded newlines have implementation-specific
        // delimiter handling. Leave these scripts to approval.
        if matches!(ch, b'[' | b']' | b'\n' | b'\r') {
            return false;
        }
        if ch == b'\\' {
            let Some((_, tail)) = rest.split_first() else {
                return false;
            };
            *rest = tail;
        }
    }
    false
}
