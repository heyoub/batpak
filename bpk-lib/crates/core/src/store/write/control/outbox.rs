use super::BatchAppendTicket;
use crate::coordinate::Coordinate;
use crate::event::{EventKind, EventPayload};
use crate::store::{
    AppendOptions, AppendReceipt, BatchAppendItem, CausationRef, Open, Store, StoreError,
};
use serde::Serialize;

/// Advanced producer staging buffer for batch submission.
///
/// Most callers should start with [`Store::append_typed`] or [`Store::append`].
/// `Outbox` is for producer code that needs to stage several events and flush
/// them as one batch, optionally through a [`VisibilityFence`].
///
/// [`VisibilityFence`]: crate::store::VisibilityFence
pub struct Outbox<'a> {
    store: &'a Store<Open>,
    fence_token: Option<u64>,
    items: Vec<BatchAppendItem>,
}

impl<'a> Outbox<'a> {
    pub(crate) fn new(store: &'a Store<Open>, fence_token: Option<u64>) -> Self {
        Self {
            store,
            fence_token,
            items: Vec::new(),
        }
    }

    /// Stage a new batch item with default append options and no causation.
    ///
    /// # Errors
    /// Returns any serialization or validation error raised while converting
    /// the payload into a staged [`BatchAppendItem`].
    pub fn stage(
        &mut self,
        coord: Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
    ) -> Result<&mut Self, StoreError> {
        self.stage_with_options_and_causation(
            coord,
            kind,
            payload,
            AppendOptions::default(),
            CausationRef::None,
        )
    }

    /// Stage a new batch item with explicit append options.
    ///
    /// # Errors
    /// Returns any serialization or validation error raised while converting
    /// the payload into a staged [`BatchAppendItem`].
    pub fn stage_with_options(
        &mut self,
        coord: Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
        options: AppendOptions,
    ) -> Result<&mut Self, StoreError> {
        self.stage_with_options_and_causation(coord, kind, payload, options, CausationRef::None)
    }

    /// Stage a new batch item with explicit causation and default append options.
    ///
    /// # Errors
    /// Returns any serialization or validation error raised while converting
    /// the payload into a staged [`BatchAppendItem`].
    pub fn stage_with_causation(
        &mut self,
        coord: Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
        causation: CausationRef,
    ) -> Result<&mut Self, StoreError> {
        self.stage_with_options_and_causation(
            coord,
            kind,
            payload,
            AppendOptions::default(),
            causation,
        )
    }

    /// Stage a new batch item with explicit append options and causation.
    ///
    /// # Errors
    /// Returns any serialization or validation error raised while converting
    /// the payload into a staged [`BatchAppendItem`].
    pub fn stage_with_options_and_causation(
        &mut self,
        coord: Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
        options: AppendOptions,
        causation: CausationRef,
    ) -> Result<&mut Self, StoreError> {
        let item = BatchAppendItem::new(coord, kind, payload, options, causation)?;
        self.items.push(item);
        Ok(self)
    }

    /// Stage a new batch item with a typed payload — kind derived from `T::KIND`.
    ///
    /// # Errors
    /// Returns any serialization or validation error raised while converting
    /// the payload into a staged [`BatchAppendItem`].
    pub fn stage_typed<T: EventPayload>(
        &mut self,
        coord: Coordinate,
        payload: &T,
    ) -> Result<&mut Self, StoreError> {
        self.stage_with_options_and_causation(
            coord,
            T::KIND,
            payload,
            AppendOptions::default(),
            CausationRef::None,
        )
    }

    /// Stage a typed batch item with explicit append options — kind derived from `T::KIND`.
    ///
    /// # Errors
    /// Returns any serialization or validation error raised while converting
    /// the payload into a staged [`BatchAppendItem`].
    pub fn stage_typed_with_options<T: EventPayload>(
        &mut self,
        coord: Coordinate,
        payload: &T,
        options: AppendOptions,
    ) -> Result<&mut Self, StoreError> {
        self.stage_with_options_and_causation(coord, T::KIND, payload, options, CausationRef::None)
    }

    /// Stage a typed batch item with explicit causation — kind derived from `T::KIND`.
    ///
    /// # Errors
    /// Returns any serialization or validation error raised while converting
    /// the payload into a staged [`BatchAppendItem`].
    pub fn stage_typed_with_causation<T: EventPayload>(
        &mut self,
        coord: Coordinate,
        payload: &T,
        causation: CausationRef,
    ) -> Result<&mut Self, StoreError> {
        self.stage_with_options_and_causation(
            coord,
            T::KIND,
            payload,
            AppendOptions::default(),
            causation,
        )
    }

    /// Stage a typed batch item with explicit append options and causation — kind derived from `T::KIND`.
    ///
    /// # Errors
    /// Returns any serialization or validation error raised while converting
    /// the payload into a staged [`BatchAppendItem`].
    pub fn stage_typed_with_options_and_causation<T: EventPayload>(
        &mut self,
        coord: Coordinate,
        payload: &T,
        options: AppendOptions,
        causation: CausationRef,
    ) -> Result<&mut Self, StoreError> {
        self.stage_with_options_and_causation(coord, T::KIND, payload, options, causation)
    }

    /// Stage a fully-formed batch item.
    pub fn push_item(&mut self, item: BatchAppendItem) -> &mut Self {
        self.items.push(item);
        self
    }

    /// Drain the staged items into a blocking batch append.
    ///
    /// Staged items are consumed from this [`Outbox`] before the enqueue/write
    /// path runs. Callers that need retry-after-error behavior must retain
    /// their own copy of the batch contents.
    ///
    /// # Errors
    /// Returns any enqueue, writer, fence, or batch-append error surfaced by
    /// the underlying flush path.
    pub fn flush(&mut self) -> Result<Vec<AppendReceipt>, StoreError> {
        let items = std::mem::take(&mut self.items);
        match self.fence_token {
            Some(token) => self.store.submit_batch_with_fence(items, token)?.wait(),
            None => self.store.append_batch(items),
        }
    }

    /// Drain the staged items into a nonblocking batch submission.
    ///
    /// Staged items are consumed from this [`Outbox`] before the submission is
    /// attempted. Callers that need retry-after-error behavior must retain
    /// their own copy of the batch contents.
    ///
    /// # Errors
    /// Returns any enqueue, writer, or fence error surfaced while turning the
    /// staged items into a batch submission ticket.
    pub fn submit_flush(&mut self) -> Result<BatchAppendTicket, StoreError> {
        let items = std::mem::take(&mut self.items);
        match self.fence_token {
            Some(token) => self.store.submit_batch_with_fence(items, token),
            None => self.store.submit_batch(items),
        }
    }

    /// Number of currently staged items.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// True when no items are staged.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}
