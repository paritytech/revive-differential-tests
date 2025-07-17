/// Defines wrappers around types.
///
/// For example, the macro invocation seen below:
///
/// ```rust,no_run
/// define_wrapper_type!(CaseId => usize);
/// ```
///
/// Would define a wrapper type that looks like the following:
///
/// ```rust,no_run
/// pub struct CaseId(usize);
/// ```
///
/// And would also implement a number of methods on this type making it easier
/// to use.
///
/// These wrapper types become very useful as they make the code a lot easier
/// to read.
///
/// Take the following as an example:
///
/// ```rust,no_run
/// struct State {
///     contracts: HashMap<usize, HashMap<String, Vec<u8>>>
/// }
/// ```
///
/// In the above code it's hard to understand what the various types refer to or
/// what to expect them to contain.
///
/// With these wrapper types we're able to create code that's self-documenting
/// in that the types tell us what the code is referring to. The above code is
/// transformed into
///
/// ```rust,no_run
/// struct State {
///     contracts: HashMap<CaseId, HashMap<ContractName, ContractByteCode>>
/// }
/// ```
#[macro_export]
macro_rules! define_wrapper_type {
    (
        $(#[$meta: meta])*
        $ident: ident => $ty: ty
    ) => {
        $(#[$meta])*
        pub struct $ident($ty);

        impl $ident {
            pub fn new(value: $ty) -> Self {
                Self(value)
            }

            pub fn new_from<T: Into<$ty>>(value: T) -> Self {
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
    };
}
