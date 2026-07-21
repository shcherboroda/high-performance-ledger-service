use std::fmt::Write;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Currency {
    code: &'static str,
    scale: u8,
}

impl Currency {
    pub const fn code(self) -> &'static str {
        self.code
    }

    pub const fn scale(self) -> u8 {
        self.scale
    }
}

const CURRENCIES: [Currency; 4] = [
    Currency {
        code: "PLN",
        scale: 2,
    },
    Currency {
        code: "EUR",
        scale: 2,
    },
    Currency {
        code: "USD",
        scale: 2,
    },
    Currency {
        code: "JPY",
        scale: 0,
    },
];

pub fn currency(input: &str) -> Option<Currency> {
    let normalized = input.trim().to_ascii_uppercase();
    CURRENCIES
        .iter()
        .copied()
        .find(|currency| currency.code == normalized)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoneyError {
    Malformed,
    TooManyFractionalDigits,
    Negative,
    Overflow,
}

pub fn parse_initial_balance(input: &str, scale: u8) -> Result<i64, MoneyError> {
    let amount = parse_minor_units(input, scale)?;
    if amount < 0 {
        return Err(MoneyError::Negative);
    }
    Ok(amount)
}

pub fn parse_minor_units(input: &str, scale: u8) -> Result<i64, MoneyError> {
    if input.is_empty() || input.trim() != input {
        return Err(MoneyError::Malformed);
    }
    let (negative, digits) = match input.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, input),
    };
    let (whole, fractional) = digits.split_once('.').unwrap_or((digits, ""));
    if whole.is_empty()
        || (digits.contains('.') && fractional.is_empty())
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fractional.bytes().all(|byte| byte.is_ascii_digit())
        || digits.matches('.').count() > 1
    {
        return Err(MoneyError::Malformed);
    }
    if fractional.len() > usize::from(scale) {
        return Err(MoneyError::TooManyFractionalDigits);
    }
    let factor = 10_i128.pow(u32::from(scale));
    let whole = whole.parse::<i128>().map_err(|_| MoneyError::Overflow)?;
    let fractional = if fractional.is_empty() {
        0
    } else {
        fractional
            .parse::<i128>()
            .map_err(|_| MoneyError::Overflow)?
            * 10_i128.pow(u32::from(scale) - fractional.len() as u32)
    };
    let amount = whole
        .checked_mul(factor)
        .and_then(|value| value.checked_add(fractional))
        .ok_or(MoneyError::Overflow)?;
    let amount = if negative { -amount } else { amount };
    i64::try_from(amount).map_err(|_| MoneyError::Overflow)
}

pub fn format_minor_units(amount: i64, scale: u8) -> String {
    let value = i128::from(amount);
    let negative = value < 0;
    let absolute = value.abs();
    let factor = 10_i128.pow(u32::from(scale));
    let whole = absolute / factor;
    let fractional = absolute % factor;
    let mut output = String::new();
    if negative {
        output.push('-');
    }
    write!(&mut output, "{whole}").expect("writing to String cannot fail");
    if scale > 0 {
        write!(
            &mut output,
            ".{fractional:0width$}",
            width = usize::from(scale)
        )
        .expect("writing to String cannot fail");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn currencies_normalize_and_expose_fixed_scales() {
        assert_eq!(currency(" pln ").unwrap().scale(), 2);
        assert_eq!(currency("jPy").unwrap().scale(), 0);
        assert!(currency("GBP").is_none());
    }

    #[test]
    fn parses_and_formats_decimal_minor_units() {
        assert_eq!(parse_minor_units("10.2", 2), Ok(1020));
        assert_eq!(parse_minor_units("-0.01", 2), Ok(-1));
        assert_eq!(format_minor_units(-1, 2), "-0.01");
        assert_eq!(format_minor_units(i64::MIN, 0), i64::MIN.to_string());
        assert_eq!(format_minor_units(100, 0), "100");
    }

    #[test]
    fn rejects_invalid_decimal_inputs_without_rounding() {
        assert_eq!(parse_minor_units("", 2), Err(MoneyError::Malformed));
        assert_eq!(parse_minor_units("1.", 2), Err(MoneyError::Malformed));
        assert_eq!(parse_minor_units(".1", 2), Err(MoneyError::Malformed));
        assert_eq!(
            parse_minor_units("1.001", 2),
            Err(MoneyError::TooManyFractionalDigits)
        );
        assert_eq!(parse_initial_balance("-1", 2), Err(MoneyError::Negative));
        assert_eq!(
            parse_minor_units("92233720368547758.08", 2),
            Err(MoneyError::Overflow)
        );
    }
}
