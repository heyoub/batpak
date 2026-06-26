use crate::event::{EventSourced, ProjectionStateContract, StateExtent, StateExtentCost};
use crate::store::projection::registry::ProjectionRegistry;
use crate::store::StoreError;

pub(super) fn validate_projection_state<T>(
    entity: &str,
    value: Option<&T>,
) -> Result<(), StoreError>
where
    T: EventSourced + 'static,
{
    let projection = ProjectionRegistry::id_for_type::<T>(entity);
    match T::STATE_CONTRACT {
        ProjectionStateContract::Unspecified => {
            Err(StoreError::ProjectionStateContractUnspecified { projection })
        }
        ProjectionStateContract::UnboundedDeclared { .. } => Ok(()),
        declared @ ProjectionStateContract::Bounded {
            max_cardinality, ..
        } => {
            let actual = value.map_or(
                StateExtent::cardinality(0, StateExtentCost::ConstantTime),
                EventSourced::state_extent,
            );
            let Some(cardinality) = actual.cardinality else {
                return Err(StoreError::ProjectionStateExtentUnavailable {
                    projection,
                    declared: Box::new(declared),
                    actual,
                });
            };
            if actual.cost == StateExtentCost::Unavailable {
                return Err(StoreError::ProjectionStateExtentUnavailable {
                    projection,
                    declared: Box::new(declared),
                    actual,
                });
            }
            if cardinality > max_cardinality {
                return Err(StoreError::ProjectionStateBoundExceeded {
                    projection,
                    declared: Box::new(declared),
                    actual,
                });
            }
            Ok(())
        }
    }
}
