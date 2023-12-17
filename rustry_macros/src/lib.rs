#![feature(proc_macro_span)]
#![feature(slice_take)]

mod harness; // TODO wat do ?

use proc_macro::{Span, TokenStream};
use proc_macro2::Ident;
use quote::{quote, ToTokens};
use rustry_test::compilers::{
    builder::{BinError, Compiler, CompilerError, CompilerKinds},
    huff::huffc::HuffcOut,
    solidity::{
        solc::{self, EntryUtils, SolcOut},
        types::internal_to_type,
    },
    vyper::vyperc::VypercOut,
};
use std::{collections::HashMap, iter};
use syn::{parse_macro_input, Error, ItemFn};

/// # Examples
///
/// ```
/// use rustry_macros::rustry_test;
///
/// fn set_up() {
///     // let counter = deploy_contract("src/Counter.sol:Counter");
/// }
///
/// #[rustry_test(set_up)]
/// fn test_increment() {
///     // if annotated with `#[rustry_test]` and that there is a set_up function,
///     // the content of the `set_up` will be copy/pasted to each rustry_test.
///     // counter.increment().send().await;
///     // assert_eq!(counter.number(), 1);
/// }
///
/// #[rustry_test(set_up)]
/// fn testFuzz_set_number(x: U256) {
///     // counter.setNumber(x).send().await;
///     // assert_eq!(counter.number(), x);
/// }
/// ```
#[proc_macro_attribute]
pub fn rustry_test(args: TokenStream, input: TokenStream) -> TokenStream {
    let mut set_up_name = None;
    let set_up_parser = syn::meta::parser(|meta| {
        if set_up_name.is_some() {
            return Err(Error::new_spanned(
                args.clone().to_string(),
                "should have only one function name",
            ));
        } else {
            set_up_name = Some(meta.path);
        }
        Ok(())
    });
    let ar = args.clone();
    parse_macro_input!(ar with set_up_parser);

    let span = Span::call_site();
    let macro_path = span.source_file().path().canonicalize().unwrap();
    let code = std::fs::read_to_string(macro_path).unwrap();
    let syntax = syn::parse_file(&code).unwrap();
    let set_up_block = if let Some(fname) = set_up_name {
        if let Some(set_up_fn) = syntax.items.into_iter().find(|item| {
            if let syn::Item::Fn(_fn) = item {
                _fn.sig.ident == fname.clone().into_token_stream().to_string()
            } else {
                false
            }
        }) {
            match set_up_fn {
                syn::Item::Fn(syn::ItemFn { block, .. }) => {
                    let block: syn::Block = *block;
                    let stmts = block.stmts;
                    quote! {
                        #(#stmts)*
                    }
                }
                _ => unreachable!(),
            }
        } else {
            syn::Error::new_spanned(fname, "invalid set_up function name").to_compile_error()
        }
    } else {
        proc_macro2::TokenStream::new()
    };
    let fun = parse_macro_input!(input as ItemFn);
    let fname = fun.sig.ident;
    let block = fun.block;

    let def = default_set_up();

    quote! {
        // #[tokio::test]
        #[test]
        pub fn #fname() {
            #def
            #set_up_block
            #block
        }
    }
    .into()
}

// TODO figure out the source mappings
#[proc_macro]
pub fn solidity(input: TokenStream) -> TokenStream {
    let lit_str = parse_macro_input!(input as syn::LitStr);
    let source_code = lit_str.value();

    let solc = Compiler {
        kind: CompilerKinds::Solc,
        sources: HashMap::from([(String::from("source_code.sol"), source_code.clone())]),
    };

    match solc.run() {
        Ok(out) => {
            let solc_out = SolcOut::try_from(out).unwrap();
            let contracts = solc_out.contracts.unwrap();
            let contract = contracts
                .get("source_code.sol")
                .unwrap()
                .get("Counter")
                .unwrap();

            let bytecode = &contract
                .evm
                .as_ref()
                .unwrap()
                .bytecode
                .as_ref()
                .unwrap()
                .object;

            let functions: Vec<_> = contract
                .abi
                .as_ref()
                .unwrap()
                .iter()
                .filter(|entry| entry.entry_type == "function")
                .collect();

            // ugly shit, use Iterator::partition
            let mut names_occur: HashMap<String, usize> = HashMap::new();
            functions.iter().for_each(|func| {
                let entry = names_occur.entry(func.name.clone()).or_default();
                *entry += 1;
            });
            let mut names_occur: HashMap<_, _> =
                names_occur.into_iter().filter(|(_, v)| *v > 1).collect();
            let functions: Vec<(&solc::AbiEntry, Ident)> = functions
                .into_iter()
                .map(|func| {
                    let new_name = if let Some(count) = names_occur.get_mut(&func.name) {
                        *count -= 1;
                        format!("{}{count}", func.name)
                    } else {
                        func.name.clone()
                    };

                    (func, Ident::new(&new_name, proc_macro2::Span::call_site()))
                })
                .rev()
                .collect();

            let impl_fns = functions.iter().map(|(func, meth_name)| {
                let signature: proc_macro2::TokenStream = func.signature().parse().unwrap();
                let inputs_w_types = func.inputs.iter().map(|input| {
                    let iname: proc_macro2::TokenStream = input.name.clone().parse().unwrap();
                    let itype: proc_macro2::TokenStream =
                        internal_to_type(&input.type_type).parse().unwrap();
                    quote! {
                        #iname: #itype
                    }
                });

                // let outputs = func.outputs.iter().map(|output| {
                //     let otype: proc_macro2::TokenStream =
                //         internal_to_type(&output.type_type).parse().unwrap();
                //     quote! {
                //         #otype
                //     }
                // });

                let fn_call = match func.state_mutability.as_str() {
                    "nonpayable" => quote! {
                        provider.call(self.address, abi_encode_signature(stringify!(#signature), vec![]).into());
                    },
                    "view" => quote! {
                        let ret = provider.staticcall(
                            self.address, 
                            abi_encode_signature(stringify!(#signature), vec![]).into()
                        );
                    },
                    _ => unimplemented!(),
                };

                let mut outputs = func.outputs.iter();
                let (output, fn_ret) = if let Some(output) = outputs.next() {
                    let output: proc_macro2::TokenStream = internal_to_type(&output.type_type).parse().unwrap();
                    if outputs.next().is_some() {
                        return syn::Error::new_spanned(
                            lit_str.clone(), 
                            "cannot use > 1 output param"
                        ).to_compile_error();
                    }

                    // let fn_ret = func.outputs.iter().map(|_| 0u128);
                    // TODO let fn_ret = func.outputs.iter().map(|_| revm::primitives::U256::ZERO);
                    // let output = func.outputs[0];
                    (
                        quote! {
                            // TODO once we support non U256 types
                            // stringify!(#output)
                            U256
                        },
                        quote! {
                            let data = ret.get_data();
                            U256::from_be_bytes::<32>(abi_decode(data, vec![AbiType::Uint]).try_into().unwrap())
                        }
                    )
                } else {
                    (
                        quote! { () },
                        proc_macro2::TokenStream::new()
                    )
                };

                quote! {
                    #[allow(clippy::unused_unit)]
                    pub fn #meth_name<'a>(
                        &self,
                        provider: &'a mut rustry_test::provider::Provider,
                        #(#inputs_w_types),*
                    // ) -> (#(#outputs),*) {
                    ) -> #output {
                        #fn_call

                        // (#(#fn_ret),*)
                        #fn_ret
                    }
                    // pub fn #meth_name() {}
                }
            });

            make_contract_instance(impl_fns, bytecode)
        }
        Err(err) => match err {
            CompilerError::BuilderError(_) => todo!(),
            CompilerError::BinError(err) => match err {
                BinError::Json(json_err) => {
                    Error::new_spanned(source_code, json_err.message).to_compile_error()
                }
            },
        },
    }
    .into()
}

#[proc_macro]
pub fn vyper(input: TokenStream) -> TokenStream {
    let lit_str = parse_macro_input!(input as syn::LitStr);
    let source_code = lit_str.value();

    let vyperc = Compiler {
        kind: CompilerKinds::Vyper,
        sources: HashMap::from([(String::from("source_code.vy"), source_code.clone())]),
    };

    match vyperc.run() {
        Ok(out) => {
            let vyc_out = VypercOut::try_from(out).unwrap();
            let contracts = vyc_out.contracts.unwrap();
            let contract = contracts
                .get("source_code.vy")
                .unwrap()
                .get("source_code")
                .unwrap();

            let bytecode = &contract
                .evm
                .as_ref()
                .unwrap()
                .bytecode
                .as_ref()
                .unwrap()
                .object
                .trim_start_matches("0x")
                .to_string();

            make_contract_instance(iter::empty::<proc_macro2::TokenStream>(), bytecode)
        }
        Err(err) => match err {
            CompilerError::BuilderError(_) => todo!(),
            CompilerError::BinError(err) => match err {
                BinError::Json(json_err) => {
                    Error::new_spanned(source_code, json_err.message).to_compile_error()
                }
            },
        },
    }
    .into()
}

#[proc_macro]
pub fn huff(input: TokenStream) -> TokenStream {
    let lit_str = parse_macro_input!(input as syn::LitStr);
    let input_str = lit_str.value();

    // heuristics for checking whether we're referencing a file or raw code
    let source_code = if input_str.ends_with(".huff") {
        std::fs::read_to_string(&input_str)
            .unwrap_or_else(|_| panic!("Unable to read file: {}", input_str))
    } else {
        input_str
    };

    let huffc = Compiler {
        kind: CompilerKinds::Huff,
        sources: HashMap::from([(String::from("source_code.huff"), source_code.clone())]),
    };

    match huffc.run() {
        Ok(out) => {
            let huffc_out = HuffcOut::try_from(out).unwrap();
            let bytecode = huffc_out.bytecode;

            make_contract_instance(iter::empty::<proc_macro2::TokenStream>(), &bytecode)
        }
        Err(err) => panic!("{:?}", err),
    }
    .into()
}

fn default_set_up() -> proc_macro2::TokenStream {
    quote! {
        let provider = 0;
    }
}

fn make_contract_instance(
    impl_fns: impl Iterator<Item = proc_macro2::TokenStream>,
    bytecode: &String,
) -> proc_macro2::TokenStream {
    quote! {
        {
            #[derive(Default, Debug)]
            struct ContractMethods {
                pub address: revm::primitives::Address,
            };

            impl ContractMethods {
                fn new(address: revm::primitives::Address) -> Self {
                    Self {
                        address
                    }
                }

                #(
                    #impl_fns
                 )*
            }

            #[derive(Default, Debug)]
            struct ContractInstance {
                pub code: revm::primitives::Bytes,
            }

            impl ContractInstance {
                fn new(code: revm::primitives::Bytes) -> Self {
                    Self {
                        code,
                    }
                }

                fn deploy<'a>(self, provider: &'a mut rustry_test::provider::Provider) -> DeployedContract {
                    let address = provider.deploy(self.code).unwrap();
                    DeployedContract {
                        address,
                        methods: ContractMethods::new(address)
                    }
                }
            }

            struct DeployedContract {
                pub address: revm::primitives::Address,
                pub methods: ContractMethods
            }

            impl rustry_test::common::contract::Contract for DeployedContract {
                fn call(&mut self, provider: &mut rustry_test::provider::Provider, data: Vec<u8>) -> rustry_test::provider::db::ExecRes {
                    provider.call(self.address, data.into())
                }

                fn staticcall(&mut self, provider: &mut rustry_test::provider::Provider, data: Vec<u8>) -> rustry_test::provider::db::ExecRes {
                    provider.staticcall(self.address, data.into())
                }

                fn send(&mut self, provider: &mut rustry_test::provider::Provider, value: revm::primitives::alloy_primitives::Uint<256, 4>) -> rustry_test::provider::db::ExecRes {
                    provider.send(self.address, value)
                }
            }

            let as_bytes = hex::decode(#bytecode).unwrap();

            let _bytecode: revm::primitives::Bytes = as_bytes.into();

            ContractInstance::new(_bytecode)
        }
    }
}
