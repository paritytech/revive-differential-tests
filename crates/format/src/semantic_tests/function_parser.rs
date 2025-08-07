use std::io::{Read, Seek};

use anyhow::{Result, anyhow};

use revive_dt_common::define_wrapper_type;

trait ReadExt: Read + Seek {
    fn read_while(
        &mut self,
        buf: &mut Vec<u8>,
        callback: impl Fn(&u8) -> bool + Clone,
    ) -> std::io::Result<()> {
        for byte in self.bytes() {
            let byte = byte?;
            let include_byte = callback(&byte);
            if include_byte {
                buf.push(byte)
            } else {
                self.seek(std::io::SeekFrom::Current(-1))?;
                break;
            }
        }
        Ok(())
    }

    fn skip_while(&mut self, callback: impl Fn(&u8) -> bool + Clone) -> std::io::Result<()> {
        for byte in self.bytes() {
            let byte = byte?;
            let skip = callback(&byte);
            if !skip {
                self.seek(std::io::SeekFrom::Current(-1))?;
                break;
            }
        }
        Ok(())
    }
}

impl<R> ReadExt for R where R: Read + Seek {}

trait Parse: Sized {
    fn parse(token_stream: &mut (impl Read + Seek)) -> Result<Self>;

    fn peek(token_stream: &mut (impl Read + Seek)) -> Result<Self> {
        let pos = token_stream.stream_position()?;
        let this = Self::parse(token_stream);
        token_stream.seek(std::io::SeekFrom::Start(pos))?;
        this
    }
}

macro_rules! impl_parse_for_tuple {
    ($first_ident: ident $(, $($ident: ident),*)?) => {
        impl<$first_ident: Parse, $($($ident: Parse),*)?> Parse for ($first_ident, $($($ident),*)?)  {
            fn parse(token_stream: &mut (impl Read + Seek)) -> Result<Self> {
                Ok((
                    $first_ident::parse(token_stream)?,
                    $(
                        $($ident::parse(token_stream)?),*
                    )?
                ))
            }
        }

        $(impl_parse_for_tuple!( $($ident),* );)?
    };
    () => {}
}

impl_parse_for_tuple!(
    A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S, T, U, V, W, X, Y, Z
);

impl Parse for String {
    fn parse(token_stream: &mut (impl Read + Seek)) -> Result<Self> {
        let mut buffer = Vec::new();
        token_stream.read_while(&mut buffer, |char| {
            char.is_ascii_alphanumeric() || char.is_ascii_whitespace()
        })?;
        let string = String::from_utf8(buffer)?;
        if string.trim().is_empty() {
            Err(anyhow!("Parsing string resulted in an empty string"))
        } else {
            Ok(string.trim().to_owned())
        }
    }
}

impl Parse for u64 {
    fn parse(token_stream: &mut (impl Read + Seek)) -> Result<Self> {
        token_stream.skip_while(u8::is_ascii_whitespace)?;

        let mut buffer = Vec::new();
        token_stream.read_while(&mut buffer, |char| matches!(char, b'0'..=b'9'))?;
        let string = String::from_utf8(buffer)?;
        string.parse().map_err(Into::into)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct Function {
    ident: FunctionIdent,
    arg_types: Parenthesized<FunctionArgumentType, ','>,
    colon: ColonToken,
    function_arguments: Vec<FunctionArgument>,
    arrow_token: ArrowToken,
    function_returns: Vec<FunctionReturn>,
    functions_options: Vec<PostFunctionOptions>,
}

impl Parse for Function {
    fn parse(token_stream: &mut (impl Read + Seek)) -> Result<Self> {
        Ok(Self {
            ident: Parse::parse(token_stream)?,
            arg_types: Parse::parse(token_stream)?,
            colon: Parse::parse(token_stream)?,
            function_arguments: {
                let mut arguments = Vec::default();
                loop {
                    if arguments.is_empty() {
                        if FunctionArgument::peek(token_stream).is_ok() {
                            arguments.push(FunctionArgument::parse(token_stream)?);
                        }
                    } else {
                        if CommaToken::peek(token_stream).is_ok() {
                            CommaToken::parse(token_stream)?;
                            arguments.push(FunctionArgument::parse(token_stream)?);
                        } else {
                            break;
                        }
                    }
                }
                arguments
            },
            arrow_token: Parse::parse(token_stream)?,
            function_returns: {
                let mut returns = Vec::default();

                loop {
                    if returns.is_empty() || CommaToken::peek(token_stream).is_ok() {
                        if !returns.is_empty() {
                            CommaToken::parse(token_stream)?;
                        }

                        let mut buf = Vec::new();
                        token_stream
                            .read_while(&mut buf, |byte| *byte != b'\n' && *byte != b',')?;
                        if NewLineToken::peek(token_stream).is_ok() {
                            NewLineToken::parse(token_stream)?;
                        } else if CommaToken::peek(token_stream).is_ok() {
                            CommaToken::peek(token_stream)?;
                        }
                        let string = String::from_utf8(buf)?;
                        let trimmed = string.trim();
                        if trimmed.chars().all(|char| char.is_whitespace()) {
                            break;
                        } else {
                            returns.push(FunctionReturn(trimmed.to_string()));
                        }
                    } else {
                        break;
                    }
                }

                returns
            },
            functions_options: {
                let mut options = Vec::default();

                while PostFunctionOptions::peek(token_stream).is_ok() {
                    options.push(PostFunctionOptions::parse(token_stream)?)
                }

                options
            },
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct Parenthesized<T, const SEP: char>(pub Vec<T>);

impl<T, const SEP: char> Parse for Parenthesized<T, SEP>
where
    T: Parse,
{
    fn parse(token_stream: &mut (impl Read + Seek)) -> Result<Self> {
        OpenParenToken::parse(token_stream)?;

        let mut inner = Vec::new();
        loop {
            if CloseParenToken::peek(token_stream).is_ok() {
                break;
            }
            inner.push(T::parse(token_stream)?);

            let reached_the_end = CloseParenToken::peek(token_stream).is_ok();
            if reached_the_end {
                break;
            } else {
                SingleCharToken::<SEP>::parse(token_stream)?;
            }
        }

        CloseParenToken::parse(token_stream)?;

        Ok(Self(inner))
    }
}

define_wrapper_type!(
    /// A wrapper type for a function identifier token.
    #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    struct FunctionIdent(String);
);

impl Parse for FunctionIdent {
    fn parse(token_stream: &mut (impl Read + Seek)) -> Result<Self> {
        Parse::parse(token_stream).map(Self)
    }
}

define_wrapper_type!(
    /// A wrapper type for a function argument token.
    #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    struct FunctionArgumentType(String);
);

impl Parse for FunctionArgumentType {
    fn parse(token_stream: &mut (impl Read + Seek)) -> Result<Self> {
        Parse::parse(token_stream).map(Self)
    }
}

define_wrapper_type!(
    /// A wrapper type for a function argument token.
    #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    struct FunctionArgument(String);
);

impl Parse for FunctionArgument {
    fn parse(token_stream: &mut (impl Read + Seek)) -> Result<Self> {
        Parse::parse(token_stream).map(Self)
    }
}

define_wrapper_type!(
    /// A wrapper type for a function return token.
    #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    struct FunctionReturn(String);
);

impl Parse for FunctionReturn {
    fn parse(token_stream: &mut (impl Read + Seek)) -> Result<Self> {
        Parse::parse(token_stream).map(Self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
struct SingleCharToken<const CHAR: char>;

impl<const CHAR: char> Parse for SingleCharToken<CHAR> {
    fn parse(token_stream: &mut (impl Read + Seek)) -> Result<Self> {
        let mut buf = [0; 1];
        loop {
            token_stream.read(&mut buf)?;
            let [byte] = buf;
            if byte == CHAR as u8 {
                return Ok(Self);
            } else if byte.is_ascii_whitespace() {
                continue;
            } else {
                return Err(anyhow!(
                    "Invalid character encountered {} expected {}",
                    byte as char,
                    CHAR
                ));
            }
        }
    }
}

// Bit of a hack, but I do this because Rust analyzer doesn't like `SingleCharToken<'>'>` and it
// messes up with the syntax highlighting.
const GT_CHAR: char = '>';

type ColonToken = SingleCharToken<':'>;
type CommaToken = SingleCharToken<','>;
type OpenParenToken = SingleCharToken<'('>;
type CloseParenToken = SingleCharToken<')'>;
type DashToken = SingleCharToken<'-'>;
type GtToken = SingleCharToken<{ GT_CHAR }>;
type NewLineToken = SingleCharToken<'\n'>;
type SpaceToken = SingleCharToken<' '>;
type ArrowToken = (DashToken, GtToken);

macro_rules! string_literal_token {
    (
        $($ty_ident: ident => $str: expr),* $(,)?
    ) => {
        $(
            #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
            pub struct $ty_ident;

            impl Parse for $ty_ident {
                fn parse(token_stream: &mut (impl Read + Seek)) -> Result<Self> {
                    token_stream.skip_while(u8::is_ascii_whitespace)?;

                    let mut buffer = [0; $str.len()];
                    token_stream.read(&mut buffer)?;
                    while SpaceToken::peek(token_stream).is_ok() {
                        SpaceToken::parse(token_stream)?;
                    }
                    if $str.as_bytes() == buffer {
                        Ok(Self)
                    } else {
                        Err(anyhow!("Invalid string - expected {} but got {:?}", $str, str::from_utf8(&buffer)))
                    }
                }
            }
        )*
    };
}
string_literal_token! {
    GasLiteralStringToken => "gas",
    IrOptimizedLiteralStringToken => "irOptimized",
    LegacyLiteralStringToken => "legacy",
    LegacyOptimizedLiteralStringToken => "legacyOptimized",
    CodeLiteralStringToken => "code",
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PostFunctionOptions {
    IrOptimizedGasOption(IrOptimizedGasOption),
    IrOptimizedGasCodeOption(IrOptimizedGasCodeOption),
    LegacyGasOption(LegacyGasOption),
    LegacyGasCodeOption(LegacyGasCodeOption),
    LegacyOptimizedGasOption(LegacyOptimizedGasOption),
    LegacyOptimizedGasCodeOption(LegacyOptimizedGasCodeOption),
}

impl Parse for PostFunctionOptions {
    fn parse(token_stream: &mut (impl Read + Seek)) -> Result<Self> {
        if IrOptimizedGasOption::peek(token_stream).is_ok() {
            IrOptimizedGasOption::parse(token_stream).map(Self::IrOptimizedGasOption)
        } else if IrOptimizedGasCodeOption::peek(token_stream).is_ok() {
            IrOptimizedGasCodeOption::parse(token_stream).map(Self::IrOptimizedGasCodeOption)
        } else if LegacyGasOption::peek(token_stream).is_ok() {
            LegacyGasOption::parse(token_stream).map(Self::LegacyGasOption)
        } else if LegacyGasCodeOption::peek(token_stream).is_ok() {
            LegacyGasCodeOption::parse(token_stream).map(Self::LegacyGasCodeOption)
        } else if LegacyOptimizedGasOption::peek(token_stream).is_ok() {
            LegacyOptimizedGasOption::parse(token_stream).map(Self::LegacyOptimizedGasOption)
        } else if LegacyOptimizedGasCodeOption::peek(token_stream).is_ok() {
            LegacyOptimizedGasCodeOption::parse(token_stream)
                .map(Self::LegacyOptimizedGasCodeOption)
        } else {
            Err(anyhow!("Failed to parse post function options"))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
struct IrOptimizedGasOption {
    pub gas_token: GasLiteralStringToken,
    pub gas_option: IrOptimizedLiteralStringToken,
    pub colon: ColonToken,
    pub value: u64,
}

impl Parse for IrOptimizedGasOption {
    fn parse(token_stream: &mut (impl Read + Seek)) -> Result<Self> {
        Ok(Self {
            gas_token: Parse::parse(token_stream)?,
            gas_option: Parse::parse(token_stream)?,
            colon: Parse::parse(token_stream)?,
            value: Parse::parse(token_stream)?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
struct IrOptimizedGasCodeOption {
    pub gas_token: GasLiteralStringToken,
    pub gas_option: IrOptimizedLiteralStringToken,
    pub code: CodeLiteralStringToken,
    pub colon: ColonToken,
    pub value: u64,
}

impl Parse for IrOptimizedGasCodeOption {
    fn parse(token_stream: &mut (impl Read + Seek)) -> Result<Self> {
        Ok(Self {
            gas_token: Parse::parse(token_stream)?,
            gas_option: Parse::parse(token_stream)?,
            code: Parse::parse(token_stream)?,
            colon: Parse::parse(token_stream)?,
            value: Parse::parse(token_stream)?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
struct LegacyGasOption {
    pub gas_token: GasLiteralStringToken,
    pub gas_option: LegacyLiteralStringToken,
    pub colon: ColonToken,
    pub value: u64,
}

impl Parse for LegacyGasOption {
    fn parse(token_stream: &mut (impl Read + Seek)) -> Result<Self> {
        Ok(Self {
            gas_token: Parse::parse(token_stream)?,
            gas_option: Parse::parse(token_stream)?,
            colon: Parse::parse(token_stream)?,
            value: Parse::parse(token_stream)?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
struct LegacyGasCodeOption {
    pub gas_token: GasLiteralStringToken,
    pub gas_option: LegacyLiteralStringToken,
    pub code: CodeLiteralStringToken,
    pub colon: ColonToken,
    pub value: u64,
}

impl Parse for LegacyGasCodeOption {
    fn parse(token_stream: &mut (impl Read + Seek)) -> Result<Self> {
        Ok(Self {
            gas_token: Parse::parse(token_stream)?,
            gas_option: Parse::parse(token_stream)?,
            code: Parse::parse(token_stream)?,
            colon: Parse::parse(token_stream)?,
            value: Parse::parse(token_stream)?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
struct LegacyOptimizedGasOption {
    pub gas_token: GasLiteralStringToken,
    pub gas_option: LegacyOptimizedLiteralStringToken,
    pub colon: ColonToken,
    pub value: u64,
}

impl Parse for LegacyOptimizedGasOption {
    fn parse(token_stream: &mut (impl Read + Seek)) -> Result<Self> {
        Ok(Self {
            gas_token: Parse::parse(token_stream)?,
            gas_option: Parse::parse(token_stream)?,
            colon: Parse::parse(token_stream)?,
            value: Parse::parse(token_stream)?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
struct LegacyOptimizedGasCodeOption {
    pub gas_token: GasLiteralStringToken,
    pub gas_option: LegacyOptimizedLiteralStringToken,
    pub code: CodeLiteralStringToken,
    pub colon: ColonToken,
    pub value: u64,
}

impl Parse for LegacyOptimizedGasCodeOption {
    fn parse(token_stream: &mut (impl Read + Seek)) -> Result<Self> {
        Ok(Self {
            gas_token: Parse::parse(token_stream)?,
            gas_option: Parse::parse(token_stream)?,
            code: Parse::parse(token_stream)?,
            colon: Parse::parse(token_stream)?,
            value: Parse::parse(token_stream)?,
        })
    }
}

#[cfg(test)]
mod test {
    use std::io::Cursor;

    use indoc::indoc;

    use super::*;

    #[test]
    fn complex_function_can_be_parsed() {
        // Arrange
        let string = indoc!(
            r#"
            myFunction(uint256, uint64,
            )
            :
            1, 2
            , 3
            -> 1, 2, 3, 4
            gas irOptimized: 135499
            gas legacy: 137095
            gas legacyOptimized: 135823
            gas irOptimized code: 135499
            gas legacy code: 137095
            gas legacyOptimized code: 135823
            "#
        );
        let mut token_stream = Cursor::new(string);

        // Act
        let function = Function::parse(&mut token_stream);

        // Assert
        let function = function.expect("Function parsing failed");
        assert_eq!(
            function,
            Function {
                ident: FunctionIdent::new("myFunction"),
                arg_types: Parenthesized(vec![
                    FunctionArgumentType::new("uint256"),
                    FunctionArgumentType::new("uint64")
                ]),
                colon: ColonToken::default(),
                function_arguments: vec![
                    FunctionArgument::new("1"),
                    FunctionArgument::new("2"),
                    FunctionArgument::new("3")
                ],
                arrow_token: ArrowToken::default(),
                function_returns: vec![
                    FunctionReturn::new("1"),
                    FunctionReturn::new("2"),
                    FunctionReturn::new("3"),
                    FunctionReturn::new("4"),
                ],
                functions_options: vec![
                    PostFunctionOptions::IrOptimizedGasOption(IrOptimizedGasOption {
                        gas_token: Default::default(),
                        gas_option: Default::default(),
                        colon: Default::default(),
                        value: 135499
                    }),
                    PostFunctionOptions::LegacyGasOption(LegacyGasOption {
                        gas_token: Default::default(),
                        gas_option: Default::default(),
                        colon: Default::default(),
                        value: 137095
                    }),
                    PostFunctionOptions::LegacyOptimizedGasOption(LegacyOptimizedGasOption {
                        gas_token: Default::default(),
                        gas_option: Default::default(),
                        colon: Default::default(),
                        value: 135823
                    }),
                    PostFunctionOptions::IrOptimizedGasCodeOption(IrOptimizedGasCodeOption {
                        gas_token: Default::default(),
                        gas_option: Default::default(),
                        code: Default::default(),
                        colon: Default::default(),
                        value: 135499
                    }),
                    PostFunctionOptions::LegacyGasCodeOption(LegacyGasCodeOption {
                        gas_token: Default::default(),
                        gas_option: Default::default(),
                        code: Default::default(),
                        colon: Default::default(),
                        value: 137095
                    }),
                    PostFunctionOptions::LegacyOptimizedGasCodeOption(
                        LegacyOptimizedGasCodeOption {
                            gas_token: Default::default(),
                            gas_option: Default::default(),
                            code: Default::default(),
                            colon: Default::default(),
                            value: 135823
                        }
                    ),
                ]
            }
        );
    }
}
