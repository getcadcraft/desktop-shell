// SPDX-License-Identifier: Apache-2.0
//! Empty on purpose: the CadCraft desktop shell trusts only the operating system's certificate store, so no CA list
//! ships in it. Any code path that would use this list finds no roots and the TLS connection fails closed.
#![no_std]

use pki_types::CertificateDer;

pub const TLS_SERVER_ROOT_CERTS: &[CertificateDer<'static>] = &[];
