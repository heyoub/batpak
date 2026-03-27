use crate::event::EventKind;
use std::marker::PhantomData;

/// Transition<From, To, P>: a typed state change with an EventKind and payload.
/// The compiler ensures you can only create transitions from valid source states.
/// [SPEC:src/typestate/transition.rs]
pub struct Transition<From, To, P> {
    kind: EventKind,
    payload: P,
    _from: PhantomData<From>,
    _to: PhantomData<To>,
}

impl<From, To, P> Transition<From, To, P> {
    pub fn new(kind: EventKind, payload: P) -> Self {
        Self {
            kind,
            payload,
            _from: PhantomData,
            _to: PhantomData,
        }
    }

    pub fn kind(&self) -> EventKind {
        self.kind
    }
    pub fn payload(&self) -> &P {
        &self.payload
    }
    pub fn into_payload(self) -> P {
        self.payload
    }
}
