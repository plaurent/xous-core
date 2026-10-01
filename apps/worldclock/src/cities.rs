//! Built-in city table and time zone rules.
//!
//! Xous has no tz database, so each city carries its standard UTC offset and one of a handful of
//! daylight saving rules that cover the places listed here. Positions feed the sunrise/sunset
//! calculation.

/// Daylight saving rules, as in effect since 2023.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Dst {
    None,
    /// USA & Canada: 2nd Sunday of March 02:00 local to 1st Sunday of November 02:00 local
    Us,
    /// European Union & UK: last Sunday of March to last Sunday of October, 01:00 UTC
    Eu,
    /// South-east Australia: 1st Sunday of October 02:00 std to 1st Sunday of April 03:00 local
    Au,
    /// New Zealand: last Sunday of September 02:00 std to 1st Sunday of April 03:00 local
    Nz,
    /// Chile: 1st Sunday of September 04:00 UTC to 1st Sunday of April 03:00 UTC
    Chile,
    /// Israel: Friday before the last Sunday of March 02:00 std to last Sunday of October 02:00 local
    Israel,
    /// Egypt: last Friday of April 00:00 std to the end of the last Thursday of October
    Egypt,
}

pub(crate) struct City {
    pub name: &'static str,
    pub lat: f64,
    pub lon: f64,
    /// standard (winter) offset from UTC, in minutes
    pub std_min: i32,
    pub dst: Dst,
}

const fn c(name: &'static str, lat: f64, lon: f64, std_min: i32, dst: Dst) -> City {
    City { name, lat, lon, std_min, dst }
}

use Dst::*;

/// Ordered roughly west to east, so the picker list reads like a map.
pub(crate) const CITIES: &[City] = &[
    c("Apia", -13.83, -171.76, 780, None),
    c("Honolulu", 21.31, -157.86, -600, None),
    c("Anchorage", 61.22, -149.90, -540, Us),
    c("Vancouver", 49.28, -123.12, -480, Us),
    c("Seattle", 47.61, -122.33, -480, Us),
    c("San Francisco", 37.77, -122.42, -480, Us),
    c("Los Angeles", 34.05, -118.24, -480, Us),
    c("Las Vegas", 36.17, -115.14, -480, Us),
    c("Phoenix", 33.45, -112.07, -420, None),
    c("Calgary", 51.05, -114.07, -420, Us),
    c("Denver", 39.74, -104.99, -420, Us),
    c("Mexico City", 19.43, -99.13, -360, None),
    c("Dallas", 32.78, -96.80, -360, Us),
    c("Houston", 29.76, -95.37, -360, Us),
    c("Winnipeg", 49.90, -97.14, -360, Us),
    c("Chicago", 41.88, -87.63, -360, Us),
    c("Atlanta", 33.75, -84.39, -300, Us),
    c("Detroit", 42.33, -83.05, -300, Us),
    c("Miami", 25.76, -80.19, -300, Us),
    c("Toronto", 43.65, -79.38, -300, Us),
    c("Washington DC", 38.91, -77.04, -300, Us),
    c("New York", 40.71, -74.01, -300, Us),
    c("Montreal", 45.50, -73.57, -300, Us),
    c("Boston", 42.36, -71.06, -300, Us),
    c("Panama", 8.98, -79.52, -300, None),
    c("Bogota", 4.71, -74.07, -300, None),
    c("Lima", -12.05, -77.04, -300, None),
    c("Caracas", 10.48, -66.90, -240, None),
    c("San Juan", 18.47, -66.11, -240, None),
    c("Halifax", 44.65, -63.58, -240, Us),
    c("Santiago", -33.45, -70.67, -240, Chile),
    c("St. John's", 47.56, -52.71, -210, Us),
    c("Buenos Aires", -34.60, -58.38, -180, None),
    c("Montevideo", -34.90, -56.16, -180, None),
    c("Sao Paulo", -23.55, -46.63, -180, None),
    c("Rio de Janeiro", -22.91, -43.17, -180, None),
    c("Nuuk", 64.18, -51.72, -120, Eu),
    c("Reykjavik", 64.15, -21.94, 0, None),
    c("UTC (Greenwich)", 51.48, 0.0, 0, None),
    c("Accra", 5.60, -0.19, 0, None),
    c("Lisbon", 38.72, -9.14, 0, Eu),
    c("Dublin", 53.35, -6.26, 0, Eu),
    c("London", 51.51, -0.13, 0, Eu),
    c("Casablanca", 33.57, -7.59, 60, None),
    c("Lagos", 6.52, 3.38, 60, None),
    c("Madrid", 40.42, -3.70, 60, Eu),
    c("Barcelona", 41.39, 2.17, 60, Eu),
    c("Paris", 48.86, 2.35, 60, Eu),
    c("Brussels", 50.85, 4.35, 60, Eu),
    c("Amsterdam", 52.37, 4.90, 60, Eu),
    c("Geneva", 46.20, 6.14, 60, Eu),
    c("Zurich", 47.38, 8.54, 60, Eu),
    c("Milan", 45.46, 9.19, 60, Eu),
    c("Rome", 41.90, 12.50, 60, Eu),
    c("Munich", 48.14, 11.58, 60, Eu),
    c("Berlin", 52.52, 13.40, 60, Eu),
    c("Copenhagen", 55.68, 12.57, 60, Eu),
    c("Oslo", 59.91, 10.75, 60, Eu),
    c("Stockholm", 59.33, 18.07, 60, Eu),
    c("Prague", 50.08, 14.44, 60, Eu),
    c("Vienna", 48.21, 16.37, 60, Eu),
    c("Budapest", 47.50, 19.04, 60, Eu),
    c("Krakow", 50.06, 19.94, 60, Eu),
    c("Warsaw", 52.23, 21.01, 60, Eu),
    c("Belgrade", 44.79, 20.45, 60, Eu),
    c("Cape Town", -33.92, 18.42, 120, None),
    c("Johannesburg", -26.20, 28.05, 120, None),
    c("Athens", 37.98, 23.73, 120, Eu),
    c("Sofia", 42.70, 23.32, 120, Eu),
    c("Bucharest", 44.43, 26.10, 120, Eu),
    c("Helsinki", 60.17, 24.94, 120, Eu),
    c("Tallinn", 59.44, 24.75, 120, Eu),
    c("Riga", 56.95, 24.11, 120, Eu),
    c("Vilnius", 54.69, 25.28, 120, Eu),
    c("Kyiv", 50.45, 30.52, 120, Eu),
    c("Cairo", 30.04, 31.24, 120, Egypt),
    c("Tel Aviv", 32.09, 34.78, 120, Israel),
    c("Jerusalem", 31.77, 35.21, 120, Israel),
    c("Istanbul", 41.01, 28.98, 180, None),
    c("Moscow", 55.76, 37.62, 180, None),
    c("Nairobi", -1.29, 36.82, 180, None),
    c("Addis Ababa", 9.03, 38.74, 180, None),
    c("Baghdad", 33.31, 44.36, 180, None),
    c("Riyadh", 24.71, 46.68, 180, None),
    c("Tehran", 35.69, 51.39, 210, None),
    c("Baku", 40.41, 49.87, 240, None),
    c("Dubai", 25.20, 55.27, 240, None),
    c("Kabul", 34.56, 69.21, 270, None),
    c("Karachi", 24.86, 67.01, 300, None),
    c("Tashkent", 41.30, 69.24, 300, None),
    c("Almaty", 43.24, 76.89, 300, None),
    c("Mumbai", 19.08, 72.88, 330, None),
    c("Delhi", 28.61, 77.21, 330, None),
    c("Bangalore", 12.97, 77.59, 330, None),
    c("Colombo", 6.93, 79.86, 330, None),
    c("Kolkata", 22.57, 88.36, 330, None),
    c("Kathmandu", 27.72, 85.32, 345, None),
    c("Dhaka", 23.81, 90.41, 360, None),
    c("Yangon", 16.84, 96.17, 390, None),
    c("Bangkok", 13.76, 100.50, 420, None),
    c("Jakarta", -6.21, 106.85, 420, None),
    c("Hanoi", 21.03, 105.85, 420, None),
    c("Ho Chi Minh City", 10.82, 106.63, 420, None),
    c("Kuala Lumpur", 3.14, 101.69, 480, None),
    c("Singapore", 1.35, 103.82, 480, None),
    c("Perth", -31.95, 115.86, 480, None),
    c("Hong Kong", 22.32, 114.17, 480, None),
    c("Shenzhen", 22.54, 114.06, 480, None),
    c("Manila", 14.60, 120.98, 480, None),
    c("Taipei", 25.03, 121.57, 480, None),
    c("Shanghai", 31.23, 121.47, 480, None),
    c("Beijing", 39.90, 116.41, 480, None),
    c("Seoul", 37.57, 126.98, 540, None),
    c("Osaka", 34.69, 135.50, 540, None),
    c("Tokyo", 35.68, 139.69, 540, None),
    c("Darwin", -12.46, 130.84, 570, None),
    c("Adelaide", -34.93, 138.60, 570, Au),
    c("Brisbane", -27.47, 153.03, 600, None),
    c("Sydney", -33.87, 151.21, 600, Au),
    c("Melbourne", -37.81, 144.96, 600, Au),
    c("Hobart", -42.88, 147.33, 600, Au),
    c("Guam", 13.44, 144.79, 600, None),
    c("Noumea", -22.27, 166.46, 660, None),
    c("Auckland", -36.85, 174.76, 720, Nz),
    c("Wellington", -41.29, 174.78, 720, Nz),
    c("Suva", -18.14, 178.44, 720, None),
    c("Kiritimati", 1.87, -157.36, 840, None),
];

pub(crate) fn find(name: &str) -> Option<&'static City> { CITIES.iter().find(|c| c.name == name) }

/// The city's offset from UTC at the given instant, in minutes, including daylight saving.
pub(crate) fn offset_min(city: &City, utc: i64) -> i32 {
    if dst_active(city.dst, city.std_min, utc) { city.std_min + 60 } else { city.std_min }
}

const DAY: i64 = 86400;
const HOUR: i64 = 3600;

/// Days since 1970-01-01 of a civil date (proleptic Gregorian; Howard Hinnant's algorithm).
pub(crate) fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y as i64 - 1 } else { y as i64 };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Civil date (year, month 1..=12, day 1..=31) of a day number since 1970-01-01.
pub(crate) fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = (yoe + era * 400 + if m <= 2 { 1 } else { 0 }) as i32;
    (y, m, d)
}

/// Day of the week, 0 = Sunday.
pub(crate) fn weekday(days: i64) -> u32 { (days + 4).rem_euclid(7) as u32 }

/// Day number of the `n`th (1-based) `wd` weekday of a month.
fn nth_weekday(y: i32, m: u32, wd: u32, n: u32) -> i64 {
    let first = days_from_civil(y, m, 1);
    let shift = (wd as i64 - weekday(first) as i64).rem_euclid(7);
    first + shift + 7 * (n as i64 - 1)
}

/// Day number of the last `wd` weekday of a month.
fn last_weekday(y: i32, m: u32, wd: u32) -> i64 {
    let next = if m == 12 { days_from_civil(y + 1, 1, 1) } else { days_from_civil(y, m + 1, 1) };
    let last = next - 1;
    last - (weekday(last) as i64 - wd as i64).rem_euclid(7)
}

const SUN: u32 = 0;
const THU: u32 = 4;
const FRI: u32 = 5;

fn dst_active(rule: Dst, std_min: i32, utc: i64) -> bool {
    let std = std_min as i64 * 60;
    let (y, _, _) = civil_from_days((utc + std).div_euclid(DAY));
    // (start, end) of daylight time in UTC seconds; `start > end` for the southern hemisphere
    let (start, end) = match rule {
        Dst::None => return false,
        Dst::Us => {
            (nth_weekday(y, 3, SUN, 2) * DAY + 2 * HOUR - std, nth_weekday(y, 11, SUN, 1) * DAY + HOUR - std)
        }
        Dst::Eu => (last_weekday(y, 3, SUN) * DAY + HOUR, last_weekday(y, 10, SUN) * DAY + HOUR),
        Dst::Au => (
            nth_weekday(y, 10, SUN, 1) * DAY + 2 * HOUR - std,
            nth_weekday(y, 4, SUN, 1) * DAY + 2 * HOUR - std,
        ),
        Dst::Nz => {
            (last_weekday(y, 9, SUN) * DAY + 2 * HOUR - std, nth_weekday(y, 4, SUN, 1) * DAY + 2 * HOUR - std)
        }
        Dst::Chile => {
            (nth_weekday(y, 9, SUN, 1) * DAY + 4 * HOUR, nth_weekday(y, 4, SUN, 1) * DAY + 3 * HOUR)
        }
        Dst::Israel => (
            last_weekday(y, 3, SUN) * DAY - 2 * DAY + 2 * HOUR - std,
            last_weekday(y, 10, SUN) * DAY + HOUR - std,
        ),
        Dst::Egypt => (last_weekday(y, 4, FRI) * DAY - std, last_weekday(y, 10, THU) * DAY + 23 * HOUR - std),
    };
    if start < end { utc >= start && utc < end } else { utc >= start || utc < end }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(y: i32, m: u32, d: u32, h: i64, min: i64) -> i64 {
        days_from_civil(y, m, d) * DAY + h * HOUR + min * 60
    }

    #[test]
    fn civil_round_trip() {
        for z in [-1000_i64, 0, 59, 60, 10957, 19723, 20000, 30000] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(weekday(days_from_civil(2026, 10, 1)), THU);
    }

    #[test]
    fn transitions_2026() {
        let ny = find("New York").unwrap();
        // 8 March 2026, 02:00 EST = 07:00 UTC
        assert_eq!(offset_min(ny, utc(2026, 3, 8, 6, 59)), -300);
        assert_eq!(offset_min(ny, utc(2026, 3, 8, 7, 0)), -240);
        // 1 November 2026, 02:00 EDT = 06:00 UTC
        assert_eq!(offset_min(ny, utc(2026, 11, 1, 5, 59)), -240);
        assert_eq!(offset_min(ny, utc(2026, 11, 1, 6, 0)), -300);

        let london = find("London").unwrap();
        assert_eq!(offset_min(london, utc(2026, 3, 29, 0, 59)), 0);
        assert_eq!(offset_min(london, utc(2026, 3, 29, 1, 0)), 60);
        assert_eq!(offset_min(london, utc(2026, 10, 25, 1, 0)), 0);

        let sydney = find("Sydney").unwrap();
        // DST ends 5 April 2026 03:00 AEDT = 4 April 16:00 UTC; starts 4 October 02:00 AEST = 3 Oct 16:00 UTC
        assert_eq!(offset_min(sydney, utc(2026, 4, 4, 15, 59)), 660);
        assert_eq!(offset_min(sydney, utc(2026, 4, 4, 16, 0)), 600);
        assert_eq!(offset_min(sydney, utc(2026, 10, 3, 15, 59)), 600);
        assert_eq!(offset_min(sydney, utc(2026, 10, 3, 16, 0)), 660);
        assert_eq!(offset_min(sydney, utc(2026, 1, 1, 0, 0)), 660);

        let auckland = find("Auckland").unwrap();
        // starts 27 September 2026 02:00 NZST = 26 Sep 14:00 UTC
        assert_eq!(offset_min(auckland, utc(2026, 9, 26, 13, 59)), 720);
        assert_eq!(offset_min(auckland, utc(2026, 9, 26, 14, 0)), 780);

        assert_eq!(offset_min(find("Tokyo").unwrap(), utc(2026, 7, 1, 0, 0)), 540);
    }
}
