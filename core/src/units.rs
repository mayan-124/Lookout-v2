//! Unit conversion for queries such as `10 km to miles` or `100 f in c`.
//! Currency is not included: it needs exchange rates, which the brief makes
//! optional and which would need either network access or an offline cache.

use crate::calc::format_significant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dim {
    Length,
    Mass,
    Volume,
    Time,
    Data,
    Speed,
    Area,
    Temperature,
}

struct Unit {
    /// Lowercase aliases accepted in a query.
    names: &'static [&'static str],
    label: &'static str,
    dim: Dim,
    /// Multiply by this to reach the dimension's base unit (not used for temperature).
    factor: f64,
}

macro_rules! unit {
    ($dim:ident, $label:expr, $factor:expr, [$($n:expr),+]) => {
        Unit { names: &[$($n),+], label: $label, dim: Dim::$dim, factor: $factor }
    };
}

const UNITS: &[Unit] = &[
    // Length, base metre
    unit!(
        Length,
        "mm",
        0.001,
        [
            "mm",
            "millimeter",
            "millimeters",
            "millimetre",
            "millimetres"
        ]
    ),
    unit!(
        Length,
        "cm",
        0.01,
        [
            "cm",
            "centimeter",
            "centimeters",
            "centimetre",
            "centimetres"
        ]
    ),
    unit!(
        Length,
        "m",
        1.0,
        ["m", "meter", "meters", "metre", "metres"]
    ),
    unit!(
        Length,
        "km",
        1000.0,
        ["km", "kilometer", "kilometers", "kilometre", "kilometres"]
    ),
    unit!(Length, "in", 0.0254, ["in", "inch", "inches"]),
    unit!(Length, "ft", 0.3048, ["ft", "foot", "feet"]),
    unit!(Length, "yd", 0.9144, ["yd", "yard", "yards"]),
    unit!(Length, "mi", 1609.344, ["mi", "mile", "miles"]),
    unit!(
        Length,
        "nmi",
        1852.0,
        ["nmi", "nautical mile", "nautical miles"]
    ),
    // Mass, base kilogram
    unit!(Mass, "mg", 1e-6, ["mg", "milligram", "milligrams"]),
    unit!(Mass, "g", 0.001, ["g", "gram", "grams"]),
    unit!(
        Mass,
        "kg",
        1.0,
        ["kg", "kilogram", "kilograms", "kilo", "kilos"]
    ),
    unit!(
        Mass,
        "t",
        1000.0,
        ["t", "tonne", "tonnes", "metric ton", "metric tons"]
    ),
    unit!(Mass, "oz", 0.028_349_523_125, ["oz", "ounce", "ounces"]),
    unit!(Mass, "lb", 0.453_592_37, ["lb", "lbs", "pound", "pounds"]),
    unit!(Mass, "st", 6.350_293_18, ["st", "stone", "stones"]),
    // Volume, base litre (US customary)
    unit!(
        Volume,
        "ml",
        0.001,
        [
            "ml",
            "milliliter",
            "milliliters",
            "millilitre",
            "millilitres"
        ]
    ),
    unit!(
        Volume,
        "l",
        1.0,
        ["l", "liter", "liters", "litre", "litres"]
    ),
    unit!(
        Volume,
        "tsp",
        0.004_928_921_593_75,
        ["tsp", "teaspoon", "teaspoons"]
    ),
    unit!(
        Volume,
        "tbsp",
        0.014_786_764_781_25,
        ["tbsp", "tablespoon", "tablespoons"]
    ),
    unit!(
        Volume,
        "fl oz",
        0.029_573_529_562_5,
        ["floz", "fl oz", "fluid ounce", "fluid ounces"]
    ),
    unit!(Volume, "cup", 0.236_588_236_5, ["cup", "cups"]),
    unit!(Volume, "pt", 0.473_176_473, ["pt", "pint", "pints"]),
    unit!(Volume, "qt", 0.946_352_946, ["qt", "quart", "quarts"]),
    unit!(Volume, "gal", 3.785_411_784, ["gal", "gallon", "gallons"]),
    // Time, base second
    unit!(Time, "ms", 0.001, ["ms", "millisecond", "milliseconds"]),
    unit!(Time, "s", 1.0, ["s", "sec", "secs", "second", "seconds"]),
    unit!(Time, "min", 60.0, ["min", "mins", "minute", "minutes"]),
    unit!(Time, "h", 3600.0, ["h", "hr", "hrs", "hour", "hours"]),
    unit!(Time, "d", 86_400.0, ["d", "day", "days"]),
    unit!(Time, "wk", 604_800.0, ["wk", "week", "weeks"]),
    unit!(Time, "yr", 31_557_600.0, ["yr", "yrs", "year", "years"]),
    // Data, base byte (kB = 1000 B, KiB = 1024 B)
    unit!(Data, "bit", 0.125, ["bit", "bits"]),
    unit!(Data, "byte", 1.0, ["byte", "bytes"]),
    unit!(Data, "kB", 1e3, ["kb", "kilobyte", "kilobytes"]),
    unit!(Data, "MB", 1e6, ["mb", "megabyte", "megabytes"]),
    unit!(Data, "GB", 1e9, ["gb", "gigabyte", "gigabytes"]),
    unit!(Data, "TB", 1e12, ["tb", "terabyte", "terabytes"]),
    unit!(Data, "KiB", 1024.0, ["kib", "kibibyte", "kibibytes"]),
    unit!(Data, "MiB", 1_048_576.0, ["mib", "mebibyte", "mebibytes"]),
    unit!(
        Data,
        "GiB",
        1_073_741_824.0,
        ["gib", "gibibyte", "gibibytes"]
    ),
    unit!(
        Data,
        "TiB",
        1_099_511_627_776.0,
        ["tib", "tebibyte", "tebibytes"]
    ),
    // Speed, base metre per second
    unit!(Speed, "m/s", 1.0, ["m/s", "mps"]),
    unit!(Speed, "km/h", 1.0 / 3.6, ["km/h", "kmh", "kph", "kmph"]),
    unit!(Speed, "mph", 0.447_04, ["mph", "mi/h"]),
    unit!(Speed, "kn", 0.514_444_444, ["kn", "kt", "knot", "knots"]),
    unit!(Speed, "ft/s", 0.3048, ["ft/s", "fps"]),
    // Area, base square metre
    unit!(Area, "cm²", 1e-4, ["cm2", "sqcm", "cm²"]),
    unit!(Area, "m²", 1.0, ["m2", "sqm", "m²"]),
    unit!(Area, "km²", 1e6, ["km2", "sqkm", "km²"]),
    unit!(Area, "ha", 10_000.0, ["ha", "hectare", "hectares"]),
    unit!(Area, "ft²", 0.092_903_04, ["ft2", "sqft", "ft²"]),
    unit!(Area, "acre", 4_046.856_422_4, ["acre", "acres"]),
    // Temperature, converted through kelvin
    unit!(Temperature, "°C", 1.0, ["c", "°c", "celsius", "centigrade"]),
    unit!(Temperature, "°F", 1.0, ["f", "°f", "fahrenheit"]),
    unit!(Temperature, "K", 1.0, ["k", "kelvin"]),
];

fn find_unit(name: &str) -> Option<&'static Unit> {
    let name = name.trim().to_lowercase();
    UNITS.iter().find(|u| u.names.contains(&name.as_str()))
}

fn to_kelvin(v: f64, label: &str) -> f64 {
    match label {
        "°C" => v + 273.15,
        "°F" => (v - 32.0) * 5.0 / 9.0 + 273.15,
        _ => v,
    }
}

fn from_kelvin(k: f64, label: &str) -> f64 {
    match label {
        "°C" => k - 273.15,
        "°F" => (k - 273.15) * 9.0 / 5.0 + 32.0,
        _ => k,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Conversion {
    pub value: f64,
    pub from: &'static str,
    pub to: &'static str,
    pub result: f64,
}

impl Conversion {
    /// e.g. `6.21371 mi`
    pub fn result_text(&self) -> String {
        format!("{} {}", format_significant(self.result, 6), self.to)
    }

    /// e.g. `10 km to mi`
    pub fn summary(&self) -> String {
        format!(
            "{} {} to {}",
            format_significant(self.value, 6),
            self.from,
            self.to
        )
    }
}

/// Every way to split at a standalone " to ", " in " or " as ", leftmost
/// first. More than one is possible: in `5 in to cm` the unit "in" is not a separator.
fn split_candidates(input: &str) -> Vec<(&str, &str)> {
    let lower = input.to_ascii_lowercase(); // same byte offsets as `input`
    let mut spots: Vec<(usize, usize)> = Vec::new();
    for sep in [" to ", " in ", " as "] {
        spots.extend(lower.match_indices(sep).map(|(i, _)| (i, sep.len())));
    }
    spots.sort_unstable();
    spots
        .into_iter()
        .filter_map(|(i, len)| Some((input.get(..i)?, input.get(i + len..)?)))
        .collect()
}

fn parse_amount(left: &str) -> Option<(f64, &str)> {
    let left = left.trim();
    let end = left
        .char_indices()
        .find(|&(i, c)| {
            !(c.is_ascii_digit() || c == '.' || c == ',' || (i == 0 && (c == '-' || c == '+')))
        })
        .map_or(left.len(), |(i, _)| i);
    let (num, unit) = left.split_at(end);
    let value: f64 = num.replace(',', "").parse().ok()?;
    let unit = unit.trim();
    (!unit.is_empty()).then_some((value, unit))
}

pub fn convert(input: &str) -> Option<Conversion> {
    split_candidates(input.trim())
        .into_iter()
        .find_map(|(left, right)| convert_parts(left, right))
}

fn convert_parts(left: &str, right: &str) -> Option<Conversion> {
    let (value, from_name) = parse_amount(left)?;
    let from = find_unit(from_name)?;
    let to = find_unit(right)?;
    if from.dim != to.dim || !value.is_finite() {
        return None;
    }
    let result = if from.dim == Dim::Temperature {
        from_kelvin(to_kelvin(value, from.label), to.label)
    } else {
        value * from.factor / to.factor
    };
    result.is_finite().then_some(Conversion {
        value,
        from: from.label,
        to: to.label,
        result,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv(s: &str) -> f64 {
        convert(s).unwrap().result
    }

    #[test]
    fn length_mass_volume_time() {
        assert!((conv("10 km to miles") - 6.213_711_9).abs() < 1e-6);
        assert!((conv("5 kg in lb") - 11.023_113_1).abs() < 1e-6);
        assert!((conv("1 gallon to liters") - 3.785_411_784).abs() < 1e-9);
        assert_eq!(conv("2 hours in minutes"), 120.0);
        assert!((conv("26.2 miles to km") - 42.164_813).abs() < 1e-5);
        assert!((conv("5 in to cm") - 12.7).abs() < 1e-9);
        assert!((conv("5 cm in in") - 1.968_503_9).abs() < 1e-6);
    }

    #[test]
    fn data_units_distinguish_decimal_and_binary() {
        assert_eq!(conv("1 gb to mb"), 1000.0);
        assert_eq!(conv("1 gib to mib"), 1024.0);
        assert_eq!(conv("8 bits as bytes"), 1.0);
    }

    #[test]
    fn temperature_has_offsets() {
        assert!((conv("100 f to c") - 37.777_777_8).abs() < 1e-6);
        assert!((conv("0 c in f") - 32.0).abs() < 1e-9);
        assert!((conv("0 c to k") - 273.15).abs() < 1e-9);
        assert!((conv("-40 c to f") + 40.0).abs() < 1e-9);
    }

    #[test]
    fn number_formats_and_speed_area() {
        assert_eq!(conv("1,000 m to km"), 1.0);
        assert!((conv("60 mph to km/h") - 96.560_64).abs() < 1e-4);
        assert!((conv("1 acre to ha") - 0.404_685_642).abs() < 1e-8);
    }

    #[test]
    fn rejects_nonsense() {
        assert!(convert("kg to km").is_none());
        assert!(convert("5 kg to km").is_none());
        assert!(convert("5 foo to bar").is_none());
        assert!(convert("report to pdf").is_none());
        assert!(convert("5 kg").is_none());
        assert!(convert("").is_none());
    }

    #[test]
    fn text_forms() {
        let c = convert("10 km to miles").unwrap();
        assert_eq!(c.result_text(), "6.21371 mi");
        assert_eq!(c.summary(), "10 km to mi");
    }
}
