// SPDX-License-Identifier: Apache-2.0
//! Empty on purpose: the CadCraft shell trusts only the operating system's certificate store (on Android through
//! rustls-platform-verifier), so no CA list ships in it. Any code path that would use this list finds no roots and the
//! TLS connection fails closed.
#![no_std]

use pki_types::TrustAnchor;

pub const TLS_SERVER_ROOTS: &[TrustAnchor<'static>] = &[];
