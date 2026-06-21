//! Shared wire-to-[`Region`] mapping for NETBAT/1 query-shaped requests.

use batpak::coordinate::{Coordinate, KindFilter, Region};
use batpak::event::EventKind;

/// Reason wire region axes could not be mapped onto a substrate [`Region`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WireRegionError {
    /// An `entity`/`scope` selector was not a valid substrate coordinate.
    Coordinate {
        /// Field whose coordinate validation failed.
        field: &'static str,
        /// Human-readable validation error.
        message: String,
    },
    /// A kind filter axis was invalid.
    Kind {
        /// Field whose kind validation failed.
        field: &'static str,
        /// Human-readable validation error.
        message: String,
    },
    /// A clock-range axis was invalid.
    ClockRange {
        /// Human-readable validation error.
        message: String,
    },
}

impl std::fmt::Display for WireRegionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Coordinate { field, message } => write!(f, "{field}: {message}"),
            Self::Kind { field, message } => write!(f, "{field}: {message}"),
            Self::ClockRange { message } => write!(f, "{message}"),
        }
    }
}

/// Map optional NETBAT region axes onto a substrate [`Region`].
///
/// `entity`/`scope` use the same coordinate validation pattern as
/// [`crate::handlers::event_query_region`]: entity is validated against a
/// placeholder scope and scope against a placeholder entity.
pub(crate) fn wire_axes_to_region(
    entity: Option<&str>,
    scope: Option<&str>,
    kind_category: Option<u8>,
    kind_type_id: Option<u16>,
    start_clock: Option<u32>,
    end_clock: Option<u32>,
) -> Result<Region, WireRegionError> {
    if let Some(entity) = entity {
        Coordinate::new(entity, "hbat-wire-region").map_err(|error| {
            WireRegionError::Coordinate {
                field: "entity",
                message: error.to_string(),
            }
        })?;
    }
    if let Some(scope) = scope {
        Coordinate::new("hbat:wire-region", scope).map_err(|error| {
            WireRegionError::Coordinate {
                field: "scope",
                message: error.to_string(),
            }
        })?;
    }

    let mut region = entity.map_or_else(Region::all, Region::entity);
    if let Some(scope) = scope {
        region = region.with_scope(scope);
    }

    region = match (kind_category, kind_type_id) {
        (Some(category), Some(type_id)) => {
            let kind = EventKind::try_custom(category, type_id).map_err(|error| {
                WireRegionError::Kind {
                    field: "kind_type_id",
                    message: format!("{error:?}"),
                }
            })?;
            region.with_fact(KindFilter::Exact(kind))
        }
        (Some(category), None) if category <= 0xF => region.with_fact_category(category),
        (Some(category), None) => {
            return Err(WireRegionError::Kind {
                field: "kind_category",
                message: format!("kind_category must fit in 4 bits, got {category}"),
            });
        }
        (None, Some(_)) => {
            return Err(WireRegionError::Kind {
                field: "kind_type_id",
                message: "kind_type_id requires kind_category".to_owned(),
            });
        }
        (None, None) => region,
    };

    match (start_clock, end_clock) {
        (Some(start), Some(end)) => {
            if start > end {
                return Err(WireRegionError::ClockRange {
                    message: format!("start_clock must be <= end_clock, got {start} > {end}"),
                });
            }
            Ok(region.with_clock_range((start, end)))
        }
        (None, None) => Ok(region),
        _ => Err(WireRegionError::ClockRange {
            message: "start_clock and end_clock must both be set or both omitted".to_owned(),
        }),
    }
}
