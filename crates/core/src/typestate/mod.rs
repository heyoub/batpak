/// Typestate transition helper: `Transition<From, To, P>` restricts transition
/// endpoints to declared state markers.
pub mod transition;

pub use transition::{StateMarker, Transition};

/// define_state_machine!: generates a sealed marker trait + zero-sized state structs.
///
/// Usage:
///   define_state_machine!(lock_state_seal, LockState { Acquired, Released });
///   // Generates:
///   //   pub trait LockState: lock_state_seal::Sealed {}
///   //   pub struct Acquired;
///   //   pub struct Released;
///   //   impl LockState for Acquired {}
///   //   impl LockState for Released {}
#[macro_export]
macro_rules! define_state_machine {
    ($seal_mod:ident, $trait_name:ident { $($state:ident),+ $(,)? }) => {
        mod $seal_mod {
            pub trait Sealed {}
        }

        pub trait $trait_name: $seal_mod::Sealed {}

        $(
            #[derive(Debug, Clone, Copy, PartialEq, Eq)]
            pub struct $state;

            impl $seal_mod::Sealed for $state {}
            impl $trait_name for $state {}
            impl $crate::typestate::transition::StateMarker for $state {}
            impl $crate::typestate::transition::sealed::Sealed for $state {}
        )+
    };
}

/// define_typestate!: generates a PhantomData wrapper for typed state machines.
///
/// Usage:
///   define_typestate!(Lock<S: LockState> { holder: String });
///   // Generates `Lock<S>` with `PhantomData<S>`, data(), into_data(), new()
#[macro_export]
macro_rules! define_typestate {
    ($name:ident<$param:ident: $bound:ident> { $($field:ident: $ftype:ty),* $(,)? }) => {
        pub struct $name<$param: $bound> {
            $( $field: $ftype, )*
            _state: ::std::marker::PhantomData<$param>,
        }

        impl<$param: $bound> $name<$param> {
            pub fn new($($field: $ftype),*) -> Self {
                Self { $($field,)* _state: ::std::marker::PhantomData }
            }

            $(
                pub fn $field(&self) -> &$ftype {
                    &self.$field
                }
            )*

            pub fn data(&self) -> ($(&$ftype,)*) {
                ($(&self.$field,)*)
            }

            pub fn into_data(self) -> ($($ftype,)*) {
                ($(self.$field,)*)
            }
        }
    };
}
