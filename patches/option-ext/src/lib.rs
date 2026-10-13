// SPDX-License-Identifier: Apache-2.0
//! `Option::contains` for stable Rust — the one helper `dirs-sys` needs. CadCraft's own code, replacing the MPL-2.0
//! `option-ext` crate through `[patch.crates-io]` in the desktop shell.

pub trait OptionExt<T> {
    /// True when the option holds a value equal to `x`.
    fn contains<U>(&self, x: &U) -> bool
    where
        U: PartialEq<T>;
}

impl<T> OptionExt<T> for Option<T> {
    fn contains<U>(&self, x: &U) -> bool
    where
        U: PartialEq<T>,
    {
        match self {
            Some(v) => x == v,
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::OptionExt;

    #[test]
    fn contains() {
        let a: Option<&str> = Some("DESKTOP");
        assert!(a.contains(&"DESKTOP"));
        assert!(!a.contains(&"MUSIC"));
        assert!(!None::<&str>.contains(&"DESKTOP"));
    }
}
