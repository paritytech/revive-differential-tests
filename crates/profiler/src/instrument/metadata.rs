use crate::internal_prelude::*;

pub(crate) struct Metadata<'wasm> {
    pub exec_instruction_function_index: u32,
    pub exec_instruction_type_index: u32,
    pub weight_consumed_function_index: u32,
    pub weight_consumed_body: parser::FunctionBody<'wasm>,
    pub run_frame_function_index: u32,
    pub run_frame_type_index: u32,
    pub call_frame_layout: CallFrameLayout,
    pub transaction_result_function_index: u32,
    pub transaction_result_type_index: u32,
    pub global_count: u32,
    pub type_count: u32,
    pub function_count: u32,
    pub imported_function_count: u32,
    pub has_import_section: bool,
}

impl<'wasm> Metadata<'wasm> {
    pub(super) fn read(wasm: &'wasm [u8]) -> Result<Self> {
        let mut types = Vec::new();
        let mut function_type_indices = Vec::new();
        let mut function_bodies = Vec::new();
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
                Payload::CodeSectionEntry(body) => function_bodies.push(body),
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
        let run_frame_body = function_bodies
            .get((run_frame_function_index - imported_function_count) as usize)
            .context("Missing call frame function body")?;
        let call_frame_layout = CallFrameLayout::read(run_frame_body, &function_names)?;
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
        let instructions = function_bodies
            .get((transaction_result_function_index - imported_function_count) as usize)
            .context("Missing transaction result function body")?
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
        let weight_consumed_body = function_bodies
            .get((weight_consumed_function_index - imported_function_count) as usize)
            .context("Missing weight getter body")?
            .clone();
        for local in weight_consumed_body.get_locals_reader()? {
            ensure!(
                local?.1 == parser::ValType::I32,
                "Unsupported weight getter local type"
            );
        }
        let operators = weight_consumed_body
            .get_operators_reader()?
            .into_iter()
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for instruction in &operators {
            ensure!(
                matches!(
                    instruction,
                    WasmOperator::LocalGet { .. }
                        | WasmOperator::LocalSet { .. }
                        | WasmOperator::LocalTee { .. }
                        | WasmOperator::I32Load { .. }
                        | WasmOperator::I32Const { .. }
                        | WasmOperator::I32Mul
                        | WasmOperator::I32Add
                        | WasmOperator::Select
                        | WasmOperator::I64Load { .. }
                        | WasmOperator::I64Store { .. }
                        | WasmOperator::End
                ),
                "Unsupported weight getter instruction"
            );
        }
        let (end, operators) = operators.split_last().context("Empty weight getter")?;
        ensure!(
            matches!(end, WasmOperator::End),
            "Weight getter has no final end"
        );
        let fields = operators
            .split_inclusive(|operator| matches!(operator, WasmOperator::I64Store { .. }))
            .map(|field| -> Result<u64> {
                let [
                    WasmOperator::LocalGet { local_index: 0 },
                    expression @ ..,
                    WasmOperator::I64Store { memarg },
                ] = field
                else {
                    bail!("Unsupported weight getter output expression");
                };
                ensure!(memarg.memory == 0, "Unsupported weight output memory");
                ensure!(
                    expression.iter().all(|operator| !matches!(
                        operator,
                        WasmOperator::LocalGet { local_index: 0 }
                            | WasmOperator::LocalSet { local_index: 0 }
                            | WasmOperator::LocalTee { local_index: 0 }
                    )),
                    "Weight getter reads its output address"
                );
                Ok(memarg.offset)
            })
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            matches!(fields.as_slice(), [0, 8] | [8, 0]),
            "Unsupported weight output fields"
        );
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
            weight_consumed_body,
            run_frame_function_index,
            run_frame_type_index,
            call_frame_layout,
            transaction_result_function_index,
            transaction_result_type_index,
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
