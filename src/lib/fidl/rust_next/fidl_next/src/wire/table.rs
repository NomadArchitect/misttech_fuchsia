// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use munge::munge;

use crate::{
    u64_le, DecodeError, Decoder, DecoderExt as _, Owned, Slot, WireEnvelope, WirePointer,
};

/// A FIDL table
#[repr(C)]
pub struct WireTable<'buf> {
    len: u64_le,
    ptr: WirePointer<'buf, WireEnvelope>,
}

impl<'buf> WireTable<'buf> {
    /// Encodes that a table contains `len` values in a slot.
    pub fn encode_len(slot: Slot<'_, Self>, len: usize) {
        munge!(let Self { len: mut table_len, ptr } = slot);
        *table_len = u64_le::from_native(len.try_into().unwrap());
        WirePointer::encode_present(ptr);
    }

    /// Decodes the fields of the table with a decoding function.
    ///
    /// The decoding function receives the ordinal of the field, its slot, and the decoder.
    pub fn decode_with<D: Decoder<'buf> + ?Sized>(
        slot: Slot<'_, Self>,
        decoder: &mut D,
        f: impl Fn(i64, Slot<'_, WireEnvelope>, &mut D) -> Result<(), DecodeError>,
    ) -> Result<(), DecodeError> {
        munge!(let Self { len, mut ptr } = slot);

        let len = len.to_native();
        if WirePointer::is_encoded_present(ptr.as_mut())? {
            let mut envelopes = decoder.take_slice_slot::<WireEnvelope>(len as usize)?;
            let envelopes_ptr = envelopes.as_mut_ptr().cast::<WireEnvelope>();

            for i in 0..len as usize {
                let mut envelope = envelopes.index(i);
                if !WireEnvelope::is_encoded_zero(envelope.as_mut()) {
                    f((i + 1) as i64, envelope, decoder)?;
                }
            }

            let envelopes = unsafe { Owned::new_unchecked(envelopes_ptr) };
            WirePointer::set_decoded(ptr, envelopes);
        } else if len != 0 {
            return Err(DecodeError::InvalidOptionalSize(len));
        }

        Ok(())
    }

    /// Returns a reference to the envelope for the given ordinal, if any.
    pub fn get(&self, ordinal: usize) -> Option<&WireEnvelope> {
        if ordinal == 0 || ordinal > self.len.to_native() as usize {
            return None;
        }

        let envelope = unsafe { &*self.ptr.as_ptr().add(ordinal - 1) };
        (!envelope.is_zero()).then_some(envelope)
    }

    /// Returns a mutable reference to the envelope for the given ordinal, if any.
    pub fn get_mut(&mut self, ordinal: usize) -> Option<&mut WireEnvelope> {
        if ordinal == 0 || ordinal > self.len.to_native() as usize {
            return None;
        }

        let envelope = unsafe { &mut *self.ptr.as_ptr().add(ordinal - 1) };
        (!envelope.is_zero()).then_some(envelope)
    }
}
