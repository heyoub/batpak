pub mod transition;

pub use transition::Transition;

/// define_state_machine!: generates a sealed marker trait + zero-sized state structs.
/// [SPEC:src/typestate/mod.rs — 99 LOC of macros]
///
/// Usage:
///   define_state_machine!(LockState { Acquired, Released });
///   // Generates:
///   //   pub trait LockState: private::Sealed {}
///   //   pub struct Acquired;
///   //   pub struct Released;
///   //   impl LockState for Acquired {}
///   //   impl LockState for Released {}
#[macro_export]
macro_rules! define_state_machine {
    ($trait_name:ident { $($state:ident),+ $(,)? }) => {
        mod private {
            pub trait Sealed {}
        }

        pub trait $trait_name: private::Sealed {}

        $(
            #[derive(Debug, Clone, Copy, PartialEq, Eq)]
            pub struct $state;

            impl private::Sealed for $state {}
            impl $trait_name for $state {}
        )+
    };
}

/// define_typestate!: generates a PhantomData wrapper for typed state machines.
/// [SPEC:src/typestate/mod.rs]
///
/// Usage:
///   define_typestate!(Lock<S: LockState> { holder: String });
///   // Generates `Lock<S>` with `PhantomData<S>`, data(), into_data(), new()
#[macro_export]
macro_rules! define_typestate {
    ($name:ident<$param:ident: $bound:ident> { $($field:ident: $ftype:ty),* $(,)? }) => {
        pub struct $name<$param: $bound> {
            $( pub $field: $ftype, )*
            _state: ::std::marker::PhantomData<$param>,
        }

        impl<$param: $bound> $name<$param> {
            pub fn new($($field: $ftype),*) -> Self {
                Self { $($field,)* _state: ::std::marker::PhantomData }
            }

            pub fn data(&self) -> ($(&$ftype,)*) {
                ($(&self.$field,)*)
            }

            pub fn into_data(self) -> ($($ftype,)*) {
                ($(self.$field,)*)
            }
        }
    };
}
