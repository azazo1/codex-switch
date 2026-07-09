pub(super) fn parse_token_amount(input: &str) -> Result<i64, String> {
    let value = input.trim().replace('_', "");
    if value.is_empty() {
        return Err("不能为空".to_string());
    }
    let (number, multiplier) = match value.chars().last().unwrap_or_default() {
        'k' | 'K' => (&value[..value.len() - 1], 1_000.0),
        'm' | 'M' => (&value[..value.len() - 1], 1_000_000.0),
        'b' | 'B' => (&value[..value.len() - 1], 1_000_000_000.0),
        _ => (value.as_str(), 1.0),
    };
    let parsed = number
        .trim()
        .parse::<f64>()
        .map_err(|_| "请输入数字, 例如 1024, 64K, 1.5M, 2B".to_string())?;
    if !parsed.is_finite() || parsed < 0.0 {
        return Err("必须是非负数字".to_string());
    }
    Ok((parsed * multiplier).round() as i64)
}

pub(super) fn parse_optional_token_amount(
    label: &str,
    value: &str,
) -> Result<Option<i64>, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    parse_token_amount(value)
        .map(Some)
        .map_err(|err| format!("{label} {err}"))
}

pub(super) fn format_token_input(value: i64) -> String {
    if value >= 1_000_000_000 && value % 1_000_000_000 == 0 {
        format!("{}B", value / 1_000_000_000)
    } else if value >= 1_000_000 && value % 1_000_000 == 0 {
        format!("{}M", value / 1_000_000)
    } else if value >= 1_000 && value % 1_000 == 0 {
        format!("{}K", value / 1_000)
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_human_readable_token_amounts() {
        assert_eq!(parse_token_amount("1024").unwrap(), 1024);
        assert_eq!(parse_token_amount("230K").unwrap(), 230_000);
        assert_eq!(parse_token_amount("1.5M").unwrap(), 1_500_000);
        assert_eq!(parse_token_amount("2B").unwrap(), 2_000_000_000);
    }

    #[test]
    fn parses_optional_token_amounts() {
        assert_eq!(
            parse_optional_token_amount("输入 tokens", "64K").unwrap(),
            Some(64_000)
        );
        assert_eq!(
            parse_optional_token_amount("输入 tokens", "   ").unwrap(),
            None
        );
    }

    #[test]
    fn formats_token_amounts_for_editor() {
        assert_eq!(format_token_input(230_000), "230K");
        assert_eq!(format_token_input(1_000_000), "1M");
        assert_eq!(format_token_input(2_000_000_000), "2B");
        assert_eq!(format_token_input(1536), "1536");
    }
}
