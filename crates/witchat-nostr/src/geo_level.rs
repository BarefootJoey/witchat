//! Location channel levels, as iOS `GeohashChannelLevel`: each level is the
//! device position's geohash cut to a fixed precision.

const BASE32: &[u8] = b"0123456789bcdefghjkmnpqrstuvwxyz";

/// Narrowest first, the order iOS lists them. iOS's building level (8)
/// only hosts location notes, so it is left out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GeoLevel {
    Block,
    Neighborhood,
    City,
    Province,
    Region,
}

impl GeoLevel {
    pub const ALL: [GeoLevel; 5] = [Self::Block, Self::Neighborhood, Self::City, Self::Province, Self::Region];

    pub fn precision(self) -> usize {
        match self {
            Self::Block => 7,
            Self::Neighborhood => 6,
            Self::City => 5,
            Self::Province => 4,
            Self::Region => 2,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Neighborhood => "neighborhood",
            Self::City => "city",
            Self::Province => "province",
            Self::Region => "region",
        }
    }

    /// iOS `levelForLength`, for bookmarks and teleported cells.
    pub fn for_length(len: usize) -> Self {
        match len {
            0..=2 => Self::Region,
            3..=4 => Self::Province,
            5 => Self::City,
            6 => Self::Neighborhood,
            _ => Self::Block,
        }
    }
}

/// Neighborhood and finer: iOS sends no presence there and shows `[? people]`
/// rather than a count that would always read zero.
pub fn is_high_precision(geohash: &str) -> bool {
    geohash.len() >= 6
}

pub fn encode_geohash(lat: f64, lon: f64, precision: usize) -> String {
    let (mut lat_range, mut lon_range) = ((-90.0f64, 90.0f64), (-180.0f64, 180.0f64));
    let mut out = String::with_capacity(precision);
    let (mut bits, mut value, mut even) = (0, 0usize, true);
    while out.len() < precision {
        let (range, coord) = if even { (&mut lon_range, lon) } else { (&mut lat_range, lat) };
        let mid = (range.0 + range.1) / 2.0;
        value <<= 1;
        if coord >= mid {
            value |= 1;
            range.0 = mid;
        } else {
            range.1 = mid;
        }
        even = !even;
        bits += 1;
        if bits == 5 {
            out.push(BASE32[value] as char);
            bits = 0;
            value = 0;
        }
    }
    out
}

/// Every level's cell for a position, narrowest first.
pub fn cells_for(lat: f64, lon: f64) -> Vec<(GeoLevel, String)> {
    let full = encode_geohash(lat, lon, GeoLevel::Block.precision());
    GeoLevel::ALL.iter().map(|level| (*level, full[..level.precision()].to_string())).collect()
}

/// iOS quick join: the region cell over the main population center of the
/// device's country (from the locale, not a position fix).
pub fn quick_join_cell(region_code: &str) -> Option<&'static str> {
    let code = region_code.to_ascii_uppercase();
    crate::quick_join_table::REGION_CELLS.iter().find(|(c, _)| *c == code).map(|(_, gh)| *gh)
}

/// iOS `coverageString`: the cell's largest side at the equator.
pub fn coverage(geohash_len: usize, metric: bool) -> String {
    let meters: f64 = match geohash_len {
        0 | 1 => 5_000_000.0,
        2 => 1_250_000.0,
        3 => 156_000.0,
        4 => 39_100.0,
        5 => 4_890.0,
        6 => 1_220.0,
        7 => 153.0,
        8 => 38.2,
        9 => 4.77,
        n => 1.19 * 0.25f64.powi(n as i32 - 10),
    };
    let (value, unit) = if metric { (meters / 1000.0, "km") } else { (meters / 1609.344, "mi") };
    if value >= 100.0 {
        format!("~{value:.0} {unit}")
    } else {
        format!("~{value:.1} {unit}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_the_reference_vectors() {
        assert_eq!(encode_geohash(57.64911, 10.40744, 11), "u4pruydqqvj");
        assert_eq!(encode_geohash(40.7484, -73.9857, 7), "dr5ru6j");
        assert_eq!(encode_geohash(-33.8688, 151.2093, 5), "r3gx2");
    }

    #[test]
    fn cells_are_prefixes_narrowest_first() {
        let cells = cells_for(40.7484, -73.9857);
        let names: Vec<_> = cells.iter().map(|(l, gh)| (l.name(), gh.as_str())).collect();
        assert_eq!(
            names,
            [
                ("block", "dr5ru6j"),
                ("neighborhood", "dr5ru6"),
                ("city", "dr5ru"),
                ("province", "dr5r"),
                ("region", "dr")
            ]
        );
        assert_eq!(GeoLevel::for_length(6), GeoLevel::Neighborhood);
        assert!(is_high_precision("dr5ru6") && !is_high_precision("dr5ru"));
    }

    #[test]
    fn quick_join_uses_the_ios_table() {
        assert_eq!(quick_join_cell("us"), Some("dr"));
        assert_eq!(quick_join_cell("TR"), Some("sx"));
        assert_eq!(quick_join_cell("ZZ"), None);
        assert!(crate::quick_join_table::REGION_CELLS.iter().all(|(_, gh)| crate::is_valid_geohash(gh)));
    }

    #[test]
    fn coverage_matches_ios_wording() {
        assert_eq!(coverage(5, true), "~4.9 km");
        assert_eq!(coverage(2, true), "~1250 km");
        assert_eq!(coverage(7, false), "~0.1 mi");
    }
}
