//! The optional `#[derive(Cell)]` for `borrow_lite`.
//!
//! It is the proc-macro counterpart of the declarative `cell!` macro: instead
//! of writing two names (`cell! { shared Stats, StatsData { .. } }`) you write
//! the value struct once and the derive builds the handle beside it.
//!
//! ```ignore
//! use borrow_lite::Cell;
//!
//! #[derive(Cell)]
//! #[cell(shared)]
//! struct Stats {
//!     hits: u64,
//!     name: String,
//! }
//! ```
//!
//! This crate is an implementation detail of the `derive` feature; depend on
//! `borrow_lite` with that feature instead of on this crate directly.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{DeriveInput, Fields, LitStr, parse_macro_input, parse_quote};

/// Derive a `borrow_lite` cell handle for a struct of named fields.
///
/// The generated handle is named `<Name>Cell`. See the `borrow_lite` README for
/// the generated surface.
///
/// The handle never grants more than the struct already does: each
/// `field()`/`field_mut()` accessor has its field's visibility, and `new` is
/// only as visible as a struct literal of the value would be.
#[proc_macro_derive(Cell, attributes(cell))]
pub fn derive_cell(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand(input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// The cell kind the handle wraps, chosen by `#[cell(..)]`.
#[derive(Clone, Copy)]
enum Kind {
    Inline,
    Local,
    Shared,
}

impl Kind {
    /// The `borrow_lite` type and constructor this kind maps to.
    fn idents(&self) -> (syn::Ident, syn::Ident) {
        match self {
            Kind::Inline => (format_ident!("Inline"), format_ident!("inline")),
            Kind::Local => (format_ident!("Local"), format_ident!("local")),
            Kind::Shared => (format_ident!("Shared"), format_ident!("shared")),
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "inline" => Some(Kind::Inline),
            "local" => Some(Kind::Local),
            "shared" => Some(Kind::Shared),
            _ => None,
        }
    }
}

/// Parse `#[cell(..)]` and build the handle.
fn expand(input: DeriveInput) -> syn::Result<TokenStream2> {
    let name = &input.ident;
    // The input struct *is* the value; the derive adds the handle beside it.
    let handle_name = format_ident!("{}Cell", name);

    let kind = parse_kind(&input)?;

    let fields = match &input.data {
        syn::Data::Struct(data) => match &data.fields {
            Fields::Named(fields) => &fields.named,
            _ => {
                return Err(syn::Error::new_spanned(
                    &input.ident,
                    "`Cell` needs named fields",
                ));
            }
        },
        _ => {
            return Err(syn::Error::new_spanned(
                &input.ident,
                "`Cell` can only be derived for a struct",
            ));
        }
    };

    let field_names: Vec<&syn::Ident> = fields
        .iter()
        .map(|f| f.ident.as_ref().expect("named field"))
        .collect();
    let field_types: Vec<&syn::Type> = fields.iter().map(|f| &f.ty).collect();
    // Each accessor is exactly as visible as the field it reaches. A `pub`
    // accessor for a private field would let any caller read or overwrite it,
    // bypassing whatever invariant the struct keeps by keeping the field
    // private - and if `unsafe` code relies on that invariant, that is a
    // soundness hole, not just an encapsulation one. The accessors live in the
    // struct's own module, so a field's visibility means the same thing on
    // the accessor as on the field.
    let field_vis: Vec<&syn::Visibility> = fields.iter().map(|f| &f.vis).collect();
    let mut_names: Vec<syn::Ident> = field_names
        .iter()
        .map(|f| format_ident!("{}_mut", f))
        .collect();
    let new_vis = constructor_visibility(&field_vis);

    let (kind_ty, ctor) = kind.idents();
    // The handle inherits the input's visibility: a `pub` handle around a
    // private value would leak the value through `Deref::Target` (E0446).
    let vis = &input.vis;
    let generics = &input.generics;
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    // Cloning a `Local`/`Shared` handle is an `Rc`/`Arc` bump, which does not
    // need `#name: Clone`, so that impl carries no bound - a bound would make
    // the handle un-clonable whenever the value is, which is not the contract.
    // An `Inline` handle *owns* its value, so cloning it copies the value and
    // genuinely needs `#name: Clone`; without the bound a non-`Clone` struct
    // could not derive an inline handle at all. The bound is written with a
    // throwaway higher-ranked lifetime so that, for a struct with no generic
    // parameters, an unsatisfied bound makes the handle un-clonable instead of
    // being rejected as a trivially false `where` clause.
    let mut clone_generics = generics.clone();
    if let Kind::Inline = kind {
        clone_generics
            .make_where_clause()
            .predicates
            .push(parse_quote! {
                for<'__borrow_lite_clone> #name #ty_generics: ::core::clone::Clone
            });
    }
    let (_, _, clone_where_clause) = clone_generics.split_for_impl();

    let doc_new = format!("Build a [`{handle_name}`] from one value per field, in order.");

    Ok(quote! {
        #[doc = #doc_new]
        #vis struct #handle_name #generics (
            ::borrow_lite::__macro_support::#kind_ty<#name #ty_generics>,
        ) #where_clause;

        // Hand-written, not derived: see the note on `clone_generics` for why
        // only the `inline` kind is bounded on the value being `Clone`.
        impl #impl_generics ::core::clone::Clone for #handle_name #ty_generics #clone_where_clause {
            fn clone(&self) -> Self {
                Self(::core::clone::Clone::clone(&self.0))
            }
        }

        impl #impl_generics #handle_name #ty_generics #where_clause {
            #[doc = #doc_new]
            #[must_use]
            #[allow(dead_code)]
            #new_vis fn new(#(#field_names: #field_types),*) -> Self {
                Self(::borrow_lite::__macro_support::#ctor(
                    #name { #(#field_names),* },
                ))
            }

            /// The underlying cell, for the whole-struct API.
            #[must_use]
            pub fn handle(&self) -> &::borrow_lite::__macro_support::#kind_ty<#name #ty_generics> {
                &self.0
            }

            #(
                #[doc = concat!("Borrow `", stringify!(#field_names), "` shared.")]
                #[must_use]
                #[track_caller]
                #[allow(dead_code)]
                #field_vis fn #field_names(&self) -> impl ::core::ops::Deref<Target = #field_types> + '_ {
                    ::borrow_lite::__macro_support::ReadGuard::map(
                        self.0.borrow(),
                        |d| &d.#field_names,
                    )
                }

                #[doc = concat!("Borrow `", stringify!(#field_names), "` exclusively.")]
                #[must_use]
                #[track_caller]
                #[allow(dead_code)]
                #field_vis fn #mut_names(&self) -> impl ::core::ops::DerefMut<Target = #field_types> + '_ {
                    ::borrow_lite::__macro_support::WriteGuard::map(
                        self.0.borrow_mut(),
                        |d| &mut d.#field_names,
                    )
                }
            )*
        }

        impl #impl_generics ::core::ops::Deref for #handle_name #ty_generics #where_clause {
            type Target = ::borrow_lite::__macro_support::#kind_ty<#name #ty_generics>;

            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }
    })
}

/// The visibility of the generated `new`.
///
/// `new` builds the value with a struct literal, so it must not be callable
/// from anywhere the struct literal itself could not be written: a `pub fn new`
/// on a struct with a private field would let any caller construct it with
/// arbitrary field values, skipping the struct's own constructor. When every
/// field shares one visibility, `new` takes it. When they differ, `new` is
/// private to the struct's module - the one place every field is visible -
/// rather than guessing which of two visibilities is the narrower. A struct
/// with no fields can be written anywhere it is visible, so its `new` is `pub`
/// (the handle's own visibility still bounds it).
fn constructor_visibility(field_vis: &[&syn::Visibility]) -> TokenStream2 {
    let mut spellings = field_vis.iter().map(|vis| quote!(#vis).to_string());
    let Some(first) = spellings.next() else {
        return quote!(pub);
    };
    if spellings.all(|other| other == first) {
        let vis = field_vis[0];
        quote!(#vis)
    } else {
        TokenStream2::new()
    }
}

/// Read the `#[cell(..)]` option, defaulting to `shared`.
fn parse_kind(input: &DeriveInput) -> syn::Result<Kind> {
    let mut kind = Kind::Shared;
    for attr in &input.attrs {
        if !attr.path().is_ident("cell") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("inline") {
                kind = Kind::Inline;
                Ok(())
            } else if meta.path.is_ident("local") {
                kind = Kind::Local;
                Ok(())
            } else if meta.path.is_ident("shared") {
                kind = Kind::Shared;
                Ok(())
            } else if meta.path.is_ident("kind") {
                let value: LitStr = meta.value()?.parse()?;
                kind = Kind::from_str(&value.value()).ok_or_else(|| {
                    syn::Error::new(value.span(), "expected `inline`, `local` or `shared`")
                })?;
                Ok(())
            } else {
                Err(meta.error("unknown `cell` option; use `inline`, `local` or `shared`"))
            }
        })?;
    }
    Ok(kind)
}
