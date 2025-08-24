#[macro_export]
macro_rules! impl_for_wrapper {
    (Display, $ident: ident) => {
        impl std::fmt::Display for $ident {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                std::fmt::Display::fmt(&self.0, f)
            }
        }
    };
    (FromStr, $ident: ident) => {
        impl std::str::FromStr for $ident {
            type Err = anyhow::Error;

            fn from_str(s: &str) -> anyhow::Result<Self> {
                s.parse().map(Self).map_err(Into::into)
            }
        }
    };
}

/// Defines wrappers around types.
///
/// For example, the macro invocation seen below:
///
/// ```rust,ignore
/// define_wrapper_type!(CaseId => usize);
/// ```
///
/// Would define a wrapper type that looks like the following:
///
/// ```rust,ignore
/// pub struct CaseId(usize);
/// ```
///
/// And would also implement a number of methods on this type making it easier to use.
///
/// These wrapper types become very useful as they make the code a lot easier to read.
///
/// Take the following as an example:
///
/// ```rust,ignore
/// struct State {
///     contracts: HashMap<usize, HashMap<String, Vec<u8>>>
/// }
/// ```
///
/// In the above code it's hard to understand what the various types refer to or what to expect them
/// to contain.
///
/// With these wrapper types we're able to create code that's self-documenting in that the types
/// tell us what the code is referring to. The above code is transformed into
///
/// ```rust,ignore
/// struct State {
///     contracts: HashMap<CaseId, HashMap<ContractName, ContractByteCode>>
/// }
/// ```
///
/// Note that we follow the same syntax for defining wrapper structs but we do not permit the use of
/// generics.
#[macro_export]
macro_rules! define_wrapper_type {
    (
        $(#[$meta: meta])*
        $vis:vis struct $ident: ident($ty: ty)

        $(
            impl $($trait_ident: ident),*
        )?

        ;
    ) => {
        $(#[$meta])*
        $vis struct $ident($ty);

        impl $ident {
            pub fn new(value: impl Into<$ty>) -> Self {
                Self(value.into())
            }

            pub fn into_inner(self) -> $ty {
                self.0
            }

            pub fn as_inner(&self) -> &$ty {
                &self.0
            }
        }

        impl AsRef<$ty> for $ident {
            fn as_ref(&self) -> &$ty {
                &self.0
            }
        }

        impl AsMut<$ty> for $ident {
            fn as_mut(&mut self) -> &mut $ty {
                &mut self.0
            }
        }

        impl std::ops::Deref for $ident {
            type Target = $ty;

            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl std::ops::DerefMut for $ident {
            fn deref_mut(&mut self) -> &mut Self::Target {
                &mut self.0
            }
        }

        impl From<$ty> for $ident {
            fn from(value: $ty) -> Self {
                Self(value)
            }
        }

        impl From<$ident> for $ty {
            fn from(value: $ident) -> Self {
                value.0
            }
        }

        $(
            $(
                $crate::macros::impl_for_wrapper!($trait_ident, $ident);
            )*
        )?
    };
}

/// Technically not needed but this allows for the macro to be found in the `macros` module of the
/// crate in addition to being found in the root of the crate.
pub use {define_wrapper_type, impl_for_wrapper};
