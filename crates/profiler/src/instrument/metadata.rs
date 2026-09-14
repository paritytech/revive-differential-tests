use crate::internal_prelude::*;

pub(crate) struct Metadata {
    pub exec_instruction_function_index: u32,
    pub exec_instruction_type_index: u32,
    pub weight_consumed_function_index: u32,
    pub run_frame_function_index: u32,
    pub run_frame_type_index: u32,
    pub call_frame_layout: CallFrameLayout,
    pub transaction_result_function_index: u32,
    pub transaction_result_type_index: u32,
    pub stack_pointer_global_index: u32,
    pub global_count: u32,
    pub type_count: u32,
    pub function_count: u32,
    pub imported_function_count: u32,
    pub has_import_section: bool,
}

impl Metadata {
    pub(super) fn read(wasm: &[u8]) -> Result<Self> {
        let mut types = Vec::new();
        let mut function_type_indices = Vec::new();
        let mut globals = Vec::new();
        let mut function_names = BTreeMap::new();
        let mut stack_pointer_global_index = None;
        let mut imported_function_count = 0;
        let mut has_import_section = false;

        for payload in Parser::new(0).parse_all(wasm) {
            match payload? {
                Payload::TypeSection(section) => {
                    types = section
                        .into_iter_err_on_gc_types()
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                }
                Payload::ImportSection(section) => {
                    has_import_section = true;
                    for import in section.into_imports() {
                        let import = import?;
                        ensure!(
                            ![
                                ENTER_OPCODE,
                                EXIT_OPCODE,
                                ENTER_CALL,
                                EXIT_CALL,
                                TRANSACTION_RESULT
                            ]
                            .contains(&import.name),
                            "Wasm is already instrumented"
                        );
                        match import.ty {
                            parser::TypeRef::Func(index) => {
                                function_type_indices.push(index);
                                imported_function_count += 1;
                            }
                            parser::TypeRef::Global(global) => globals.push(global),
                            _ => {}
                        }
                    }
                }
                Payload::FunctionSection(section) => {
                    for index in section {
                        function_type_indices.push(index?);
                    }
                }
                Payload::GlobalSection(section) => {
                    for global in section {
                        globals.push(global?.ty);
                    }
                }
                Payload::CustomSection(section) => {
                    if let parser::KnownCustom::Name(section) = section.as_known() {
                        for name in section {
                            match name? {
                                parser::Name::Function(functions) => {
                                    for function in functions {
                                        let function = function?;
                                        function_names.insert(
                                            function.index,
                                            format!("{:#}", demangle(function.name)),
                                        );
                                    }
                                }
                                parser::Name::Global(globals) => {
                                    for global in globals {
                                        let global = global?;
                                        if global.name == "__stack_pointer" {
                                            stack_pointer_global_index = Some(global.index);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        let exec_instruction_function_index =
            find_unique_function_index(&function_names, |name| {
                name.contains("pallet_revive::vm::evm::instructions::exec_instruction")
            })?;
        let weight_consumed_function_index = find_unique_function_index(&function_names, |name| {
            name.contains("pallet_revive::vm::evm::interpreter::Interpreter")
                && name.contains("pallet_revive::tracing::FrameTraceInfo")
                && name.ends_with("::weight_consumed")
        })?;
        let run_frame_function_index = find_unique_function_index(&function_names, |name| {
            name.contains("pallet_revive::exec::Stack") && name.ends_with("::run")
        })?;
        let transaction_result_function_index =
            find_unique_function_index(&function_names, |name| {
                name.contains("pallet_revive::evm::block_storage::EthereumCallResult::new")
            })?;
        let validate_signature =
            |function_index: u32, parameters: &[parser::ValType]| -> Result<u32> {
                let type_index = function_type_indices
                    .get(function_index as usize)
                    .context("Function name has an invalid index")?;
                let ty = &types[*type_index as usize];
                ensure!(
                    function_index >= imported_function_count
                        && ty.params() == parameters
                        && ty.results().is_empty(),
                    "EVM function ABI differs from the supported runtime"
                );
                Ok(*type_index)
            };
        let exec_instruction_type_index =
            validate_signature(exec_instruction_function_index, &[parser::ValType::I32; 3])?;
        validate_signature(weight_consumed_function_index, &[parser::ValType::I32; 2])?;
        let run_frame_type_index =
            validate_signature(run_frame_function_index, &[parser::ValType::I32; 4])?;
        let run_frame_body = Parser::new(0)
            .parse_all(wasm)
            .filter_map(|payload| match payload {
                Ok(Payload::CodeSectionEntry(body)) => Some(body),
                _ => None,
            })
            .nth((run_frame_function_index - imported_function_count) as usize)
            .context("Missing call frame function body")?;
        let call_frame_layout = CallFrameLayout::read(&run_frame_body, &function_names)?;
        let transaction_result_type_index = validate_signature(
            transaction_result_function_index,
            &[
                parser::ValType::I32,
                parser::ValType::I32,
                parser::ValType::I32,
                parser::ValType::I64,
                parser::ValType::I64,
                parser::ValType::I32,
                parser::ValType::I32,
                parser::ValType::I32,
            ],
        )?;
        let mut function_index = imported_function_count;
        for payload in Parser::new(0).parse_all(wasm) {
            if let Payload::CodeSectionEntry(body) = payload? {
                if function_index == transaction_result_function_index {
                    let instructions = body
                        .get_operators_reader()?
                        .into_iter()
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    let result_tag = instructions.windows(4).any(|instructions| {
                        matches!(
                            instructions,
                            [parser::Operator::LocalGet { local_index: 2 },
                             parser::Operator::I32Load8U { memarg },
                             parser::Operator::I32Const { value: 15 },
                             parser::Operator::I32Ne] if memarg.offset == 112
                        )
                    });
                    let fields = [116, 120, 124].into_iter().all(|offset| {
                        instructions.windows(2).any(|instructions| {
                            matches!(
                                instructions,
                                [parser::Operator::LocalGet { local_index: 2 },
                                 parser::Operator::I32Load { memarg }]
                                    if memarg.offset == offset
                            )
                        })
                    });
                    let flags = instructions.windows(3).any(|instructions| {
                        matches!(
                            instructions,
                            [parser::Operator::LocalGet { local_index: 2 },
                             parser::Operator::I32Load8U { memarg },
                             parser::Operator::I32Const { value: 1 }]
                                if memarg.offset == 128
                        )
                    });
                    ensure!(
                        result_tag && fields && flags,
                        "Transaction result ABI differs from the supported runtime"
                    );
                }
                function_index += 1;
            }
        }
        let stack_pointer_global_index =
            stack_pointer_global_index.context("Missing __stack_pointer global")?;
        match globals.get(stack_pointer_global_index as usize) {
            Some(global) if global.mutable && global.content_type == parser::ValType::I32 => {}
            _ => bail!("Invalid __stack_pointer global"),
        }
        Ok(Self {
            exec_instruction_function_index,
            exec_instruction_type_index,
            weight_consumed_function_index,
            run_frame_function_index,
            run_frame_type_index,
            call_frame_layout,
            transaction_result_function_index,
            transaction_result_type_index,
            stack_pointer_global_index,
            global_count: globals.len() as u32,
            type_count: types.len() as u32,
            function_count: function_type_indices.len() as u32,
            imported_function_count,
            has_import_section,
        })
    }
}

pub(crate) struct CallFrameLayout {
    pub frames_pointer_offset: u64,
    pub frames_length_offset: u64,
    pub frame_size: i32,
    pub code_address_offset: i32,
}

impl CallFrameLayout {
    fn read(body: &parser::FunctionBody<'_>, names: &BTreeMap<u32, String>) -> Result<Self> {
        let instructions = body
            .get_operators_reader()?
            .into_iter()
            .collect::<std::result::Result<Vec<_>, _>>()?;
        // Recover top_frame's array access and code_address's comparison with
        // the destination. Both come from Stack::run in the compiled runtime.
        let frames = instructions
            .windows(14)
            .filter_map(|instructions| match instructions {
                [
                    WasmOperator::LocalGet { local_index: 1 },
                    WasmOperator::I32Load { memarg: pointer },
                    WasmOperator::LocalGet { local_index: 1 },
                    WasmOperator::I32Load { memarg: length },
                    WasmOperator::LocalTee {
                        local_index: length_local,
                    },
                    WasmOperator::I32Const { value: size },
                    WasmOperator::I32Mul,
                    WasmOperator::I32Add,
                    WasmOperator::I32Const {
                        value: negative_size,
                    },
                    WasmOperator::I32Add,
                    WasmOperator::LocalGet { local_index: 1 },
                    WasmOperator::LocalGet {
                        local_index: selected_length,
                    },
                    WasmOperator::Select,
                    WasmOperator::LocalTee {
                        local_index: frame_local,
                    },
                ] if *size > 0 && *negative_size == -*size && length_local == selected_length => {
                    Some((pointer.offset, length.offset, *size, *frame_local))
                }
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        ensure!(frames.len() == 1, "Unsupported call frame array layout");
        let (frames_pointer_offset, frames_length_offset, frame_size, frame_local) =
            frames.first().context("Missing call frame array access")?;
        let addresses = instructions
            .windows(9)
            .filter_map(|instructions| match instructions {
                [
                    WasmOperator::LocalGet { local_index },
                    WasmOperator::I32Const { value: offset },
                    WasmOperator::I32Add,
                    WasmOperator::LocalTee { .. },
                    WasmOperator::LocalGet { .. },
                    WasmOperator::I32Const { .. },
                    WasmOperator::I32Add,
                    WasmOperator::I32Const { value: 20 },
                    WasmOperator::Call { function_index },
                ] if local_index == frame_local
                    && *offset >= 0
                    && *offset <= *frame_size - 20
                    && names
                        .get(function_index)
                        .is_some_and(|name| name == "memcmp") =>
                {
                    Some(*offset)
                }
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        ensure!(
            addresses.len() == 1,
            "Unsupported call frame code address layout"
        );
        let code_address_offset = addresses
            .first()
            .context("Missing code address comparison")?;
        Ok(Self {
            frames_pointer_offset: *frames_pointer_offset,
            frames_length_offset: *frames_length_offset,
            frame_size: *frame_size,
            code_address_offset: *code_address_offset,
        })
    }
}

fn find_unique_function_index(
    function_names: &BTreeMap<u32, String>,
    matches: impl Fn(&str) -> bool,
) -> Result<u32> {
    let mut matches = function_names.iter().filter(|(_, name)| matches(name));
    let index = matches
        .next()
        .map(|(index, _)| *index)
        .context("Missing EVM dispatch or weight getter symbol")?;
    ensure!(
        matches.next().is_none(),
        "Ambiguous EVM dispatch or weight getter symbol"
    );
    Ok(index)
}
