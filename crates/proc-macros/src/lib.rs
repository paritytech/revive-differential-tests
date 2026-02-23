mod context;

/// Generates a CLI-integrated configuration system from a declarative module definition.
///
/// # Motivation
///
/// The retester tool has many subcommands (`test`, `benchmark`, `export-genesis`, etc.), each
/// requiring a different subset of configuration parameters (compilers, nodes, wallets, etc.).
/// Before this macro, the `Context` enum and all of its `AsRef<Config>` implementations were
/// written by hand — roughly 40 trait impls that had to be kept in sync whenever a configuration
/// was added or a subcommand changed. This macro eliminates that boilerplate entirely: you
/// declare the subcommands and their configurations, and everything else is generated.
///
/// # Usage
///
/// The macro is applied to a module containing struct definitions annotated with either
/// `#[subcommand]` or `#[configuration]`:
///
/// ```rust,ignore
/// #[revive_dt_proc_macros::context(
///     context_type_ident = "Context",
///     default_derives = "Clone, Debug, Parser, Serialize, Deserialize"
/// )]
/// #[command(name = "retester")]
/// mod context {
///     use super::*;
///
///     #[subcommand]
///     pub struct Test {
///         pub solc: SolcConfiguration,
///         pub wallet: WalletConfiguration,
///     }
///
///     #[subcommand]
///     pub struct Benchmark {
///         pub solc: SolcConfiguration,
///     }
///
///     #[subcommand]
///     pub struct ExportJsonSchema;
///
///     #[configuration(key = "solc")]
///     pub struct SolcConfiguration {
///         #[clap(default_value = "0.8.29")]
///         pub version: Version,
///     }
///
///     #[configuration(key = "wallet")]
///     pub struct WalletConfiguration {
///         #[clap(default_value_t = 100_000)]
///         pub additional_keys: usize,
///     }
///
///     impl Test {
///         pub fn custom_method(&self) { /* ... */ }
///     }
///
///     impl Default for Test {
///         fn default() -> Self {
///             Self::parse_from(["test", "--test", "."])
///         }
///     }
/// }
/// ```
///
/// # Macro arguments
///
/// | Argument | Default | Description |
/// |----------|---------|-------------|
/// | `context_type_ident` | `"Context"` | The name of the generated enum type. |
/// | `default_derives` | *(none)* | Comma-separated derive macros applied to all generated types. |
///
/// # Item annotations
///
/// ## `#[subcommand]`
///
/// Marks a struct as a CLI subcommand (a variant of the generated `Context` enum). Each field
/// must be a type annotated with `#[configuration]`. Fields are automatically flattened into the
/// CLI via `#[clap(flatten)]`.
///
/// ## `#[configuration]` / `#[configuration(key = "...")]`
///
/// Marks a struct as a configuration group. The optional `key` argument prefixes all fields in
/// the CLI. For example, `#[configuration(key = "solc")]` on a struct with a `version` field
/// produces the CLI flag `--solc.version`. A `help_heading` is automatically derived from the
/// key (e.g. `"solc"` becomes `"Solc Configuration"`), or can be specified explicitly.
///
/// # What is generated
///
/// Given the example above, the macro produces the following (simplified):
///
/// ## 1. Context enum
///
/// A sum type with one boxed variant per subcommand:
///
/// ```rust,ignore
/// pub enum Context {
///     Test(Box<Test>),
///     Benchmark(Box<Benchmark>),
///     ExportJsonSchema(Box<ExportJsonSchema>),
/// }
/// ```
///
/// Variants are boxed to prevent enum size explosion when subcommands contain many
/// configurations.
///
/// ## 2. `AsRef<Config>` implementations
///
/// For **every** (subcommand, configuration) pair, an `AsRef<Config>` impl is generated:
///
/// - If the subcommand **has** the configuration as a field, the impl returns a reference to
///   that field.
/// - If the subcommand **does not** have the configuration, the impl returns a reference to a
///   `LazyLock` static default (constructed by parsing empty CLI arguments via `clap::Parser`).
///
/// The `Context` enum also implements `AsRef<Config>` for every configuration by matching on
/// its variants and delegating.
///
/// This means **any subcommand and the `Context` enum can access any configuration**, even
/// configurations they don't structurally contain. Missing configurations simply return their
/// default values.
///
/// ## 3. `Has<Config>` accessor traits
///
/// For each configuration type, a public trait is generated with a named accessor method:
///
/// ```rust,ignore
/// // For SolcConfiguration:
/// pub trait HasSolcConfiguration {
///     fn as_solc_configuration(&self) -> &SolcConfiguration;
/// }
///
/// // For WalletConfiguration:
/// pub trait HasWalletConfiguration {
///     fn as_wallet_configuration(&self) -> &WalletConfiguration;
/// }
/// ```
///
/// These traits are implemented on every subcommand and on the `Context` enum, delegating to
/// the corresponding `AsRef` impl.
///
/// The `Has*` traits serve as ergonomic, named trait bounds for functions that need specific
/// configurations:
///
/// ```rust,ignore
/// // Instead of the generic:
/// fn new(context: impl AsRef<SolcConfiguration> + AsRef<WorkingDirectoryConfiguration>) { }
///
/// // You write the self-documenting:
/// fn new(context: impl HasSolcConfiguration + HasWorkingDirectoryConfiguration) { }
///
/// // And call with named methods instead of turbofish:
/// let solc = context.as_solc_configuration();
/// ```
///
/// **Note:** `Has*` traits are only implemented on the types themselves, not on references
/// to them. Pass configuration holders by value or clone rather than by reference.
///
/// ## 4. `Default` implementations
///
/// For subcommands that do not provide a hand-written `impl Default`, a default is
/// auto-generated by parsing empty CLI arguments:
///
/// ```rust,ignore
/// impl Default for ExportJsonSchema {
///     fn default() -> Self {
///         <Self as clap::Parser>::parse_from(std::iter::empty::<std::ffi::OsString>())
///     }
/// }
/// ```
///
/// If you provide your own `impl Default` inside the module, the macro detects it and skips
/// generation for that type.
///
/// ## 5. Compile-time safety
///
/// Inside a `const _: () = { ... }` block, the macro generates:
///
/// - A private `IsConfig` marker trait implemented for every `#[configuration]` type.
/// - A compile-time assertion that every field of every `#[subcommand]` struct is a type that
///   implements `IsConfig`. This catches mistakes like accidentally using a non-configuration
///   type as a subcommand field.
///
/// # Rules and constraints
///
/// - The module must have inline content (`mod context { ... }`), not a file reference
///   (`mod context;`).
/// - Every struct or enum in the module must be annotated with either `#[subcommand]` or
///   `#[configuration]`.
/// - `impl` blocks inside the module must target types defined within the same module (or the
///   context enum itself).
/// - A subcommand may have **at most one field** of each configuration type.
/// - Only `use` statements, structs, enums, and `impl` blocks are allowed inside the module.
#[proc_macro_attribute]
pub fn context(
    attr: proc_macro::TokenStream,
    item: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    let item = syn::parse_macro_input!(item as syn::ItemMod);
    match context::handler(attr.into(), item) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}
