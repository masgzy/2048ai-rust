//! Tiny dependency-free CLI argument parsing and `bestmove` input parsing.

use std::collections::HashMap;

/// Parsed command line: `--flag`, `--opt value`, `--opt=value`, positionals.
pub struct Options {
    map: HashMap<String, String>,
    pub positional: Vec<String>,
}

impl Options {
    /// `value_opts` lists option names that take a value.
    pub fn parse(args: &[String], value_opts: &[&str]) -> Result<Options, String> {
        let mut map = HashMap::new();
        let mut positional = Vec::new();
        let mut i = 0usize;
        while i < args.len() {
            let a = &args[i];
            if let Some(rest) = a.strip_prefix("--") {
                if rest == "help" {
                    return Err(String::from("__help__"));
                }
                if let Some((k, v)) = rest.split_once('=') {
                    validate_value_opt(k, value_opts)?;
                    map.insert(k.to_string(), v.to_string());
                } else if value_opts.contains(&rest) {
                    i += 1;
                    let v = args
                        .get(i)
                        .ok_or_else(|| format!("--{rest} requires a value"))?;
                    map.insert(rest.to_string(), v.clone());
                } else {
                    map.insert(rest.to_string(), "true".to_string());
                }
            } else {
                positional.push(a.clone());
            }
            i += 1;
        }
        Ok(Options { map, positional })
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.map.get(key).map(|s| s.as_str())
    }

    pub fn has(&self, key: &str) -> bool {
        self.map.contains_key(key)
    }

    pub fn get_or<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.get(key).unwrap_or(default)
    }

    pub fn num<T: std::str::FromStr>(&self, key: &str, default: T) -> Result<T, String> {
        match self.get(key) {
            None => Ok(default),
            Some(s) => s
                .parse::<T>()
                .map_err(|_| format!("invalid value for --{key}: '{s}'")),
        }
    }
}

fn validate_value_opt(name: &str, value_opts: &[&str]) -> Result<(), String> {
    if value_opts.contains(&name) {
        Ok(())
    } else {
        Err(format!("unknown option --{name}"))
    }
}

/// Parse a `bestmove` board from stdin text.
///
/// Accepted formats:
/// - 16 whitespace-separated displayed values, row-major (`0` = empty),
///   e.g. `2 0 0 0 0 0 0 0 0 0 0 0 0 0 0 4`;
/// - one hex u64 of packed ranks, e.g. `0x0000000000001001`.
pub fn parse_board_input(text: &str) -> Result<u64, String> {
    let trimmed = text.trim();
    if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        let v =
            u64::from_str_radix(hex, 16).map_err(|_| format!("invalid hex board: '{trimmed}'"))?;
        // Reject nibbles above 15 implicitly impossible; ranks are 4 bits.
        return Ok(v);
    }
    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    if parts.len() != 16 {
        return Err(format!(
            "expected 16 space-separated tile values or one hex board, got {} token(s)",
            parts.len()
        ));
    }
    let mut vals = [0u32; 16];
    for (i, p) in parts.iter().enumerate() {
        let v: u32 = p.parse().map_err(|_| format!("invalid tile value '{p}'"))?;
        vals[i] = v;
    }
    crate::board::board_from_values(&vals)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(args: &[&str]) -> Vec<String> {
        args.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parses_flags_and_values() {
        let opts = Options::parse(
            &s(&["--games", "4", "--json", "--seed=9", "extra"]),
            &["games", "seed"],
        )
        .unwrap();
        assert_eq!(opts.get("games"), Some("4"));
        assert_eq!(opts.get("seed"), Some("9"));
        assert!(opts.has("json"));
        assert_eq!(opts.positional, vec!["extra"]);
        assert_eq!(opts.num::<u32>("games", 1).unwrap(), 4);
        assert_eq!(opts.num::<u32>("threads", 2).unwrap(), 2);
    }

    #[test]
    fn rejects_missing_value_and_unknown_equals() {
        assert!(Options::parse(&s(&["--games"]), &["games"]).is_err());
        assert!(Options::parse(&s(&["--bogus=3"]), &["games"]).is_err());
        assert!(Options::parse(&s(&["--help"]), &[]).is_err());
    }

    #[test]
    fn parses_16_values() {
        let input = "2 0 0 0\n0 0 0 0\n0 0 0 0\n0 0 0 4\n";
        let b = parse_board_input(input).unwrap();
        let vals = crate::board::board_to_values(b);
        assert_eq!(vals[0], 2);
        assert_eq!(vals[15], 4);
        assert_eq!(vals[7], 0);
    }

    #[test]
    fn parses_hex_board() {
        // rank 1 at nibble 0, rank 2 at nibble 15
        let b = parse_board_input("0x2000000000000001").unwrap();
        let vals = crate::board::board_to_values(b);
        assert_eq!(vals[0], 2);
        assert_eq!(vals[15], 4);
    }

    #[test]
    fn rejects_bad_input_without_panicking() {
        assert!(parse_board_input("1 2 3").is_err());
        assert!(parse_board_input("3 3 3 3 3 3 3 3 3 3 3 3 3 3 3 3").is_err()); // not powers of 2
        assert!(parse_board_input("0xzz").is_err());
        assert!(parse_board_input("").is_err());
        // 65536 is out of the supported range
        assert!(parse_board_input("65536 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0").is_err());
    }
}
