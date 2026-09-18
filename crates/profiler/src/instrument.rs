use crate::internal_prelude::*;

pub(crate) mod metadata;

pub fn instrument_wasm(wasm: impl AsRef<[u8]>) -> Result<Vec<u8>> {
    let wasm = wasm.as_ref();
    Validator::new()
        .validate_all(wasm)
        .context("Invalid Wasm")?;
    let metadata = Metadata::read(wasm)?;
    let mut module = encoder::Module::new();
    Instrumenter { metadata }
        .parse_core_module(&mut module, Parser::new(0), wasm)
        .context("Failed to rewrite Wasm")?;
    let wasm = module.finish();
    Validator::new()
        .validate_all(&wasm)
        .context("Invalid instrumented Wasm")?;
    Ok(wasm)
}

struct Instrumenter<'wasm> {
    metadata: Metadata<'wasm>,
}

impl Instrumenter<'_> {
    fn original_index(&self, index: u32) -> u32 {
        if index < self.metadata.imported_function_count {
            index
        } else {
            index + HOST_FUNCTION_COUNT
        }
    }

    fn add_imports(&self, imports: &mut encoder::ImportSection) {
        for (name, type_offset) in [
            (ENTER_OPCODE, 0),
            (EXIT_OPCODE, 0),
            (ENTER_CALL, 3),
            (EXIT_CALL, 1),
            (TRANSACTION_RESULT, 2),
        ] {
            imports.import(
                "env",
                name,
                encoder::EntityType::Function(self.metadata.type_count + type_offset),
            );
        }
    }

    fn transaction_result_wrapper(&self) -> encoder::Function {
        let mut function = encoder::Function::new([]);
        function.instruction(&LocalGet(2));
        function.instruction(&Call(self.metadata.imported_function_count + 4));
        for argument in 0..8 {
            function.instruction(&LocalGet(argument));
        }
        function.instruction(&Call(
            self.original_index(self.metadata.transaction_result_function_index),
        ));
        function.instruction(&End);
        function
    }

    fn opcode_wrapper(
        &mut self,
    ) -> std::result::Result<encoder::Function, reencode::Error<Infallible>> {
        let getter = self.metadata.weight_consumed_body.clone();
        let getter_locals = getter
            .get_locals_reader()?
            .into_iter()
            .try_fold(0, |total, local| local.map(|(count, _)| total + count))?;
        let mut function = encoder::Function::new([
            (2 + getter_locals, encoder::ValType::I32),
            (2, encoder::ValType::I64),
        ]);
        let ref_time_local = 5 + getter_locals;
        let proof_size_local = ref_time_local + 1;
        let current_op_code_global = self.metadata.global_count;
        let open_op_codes_global = self.metadata.global_count + 1;
        let weight_consumed_function_index =
            self.original_index(self.metadata.weight_consumed_function_index);
        for instruction in [
            GlobalGet(current_op_code_global),
            LocalSet(3),
            LocalGet(2),
            GlobalSet(current_op_code_global),
            GlobalGet(open_op_codes_global),
            I32Const(1),
            I32Add,
            GlobalSet(open_op_codes_global),
            LocalGet(1),
            Call(weight_consumed_function_index),
            LocalGet(2),
            LocalGet(ref_time_local),
            LocalGet(proof_size_local),
            Call(self.metadata.imported_function_count),
            LocalGet(0),
            LocalGet(1),
            LocalGet(2),
            Call(self.original_index(self.metadata.exec_instruction_function_index)),
            LocalGet(1),
            Call(weight_consumed_function_index),
            LocalGet(2),
            LocalGet(ref_time_local),
            LocalGet(proof_size_local),
            Call(self.metadata.imported_function_count + 1),
            LocalGet(3),
            GlobalSet(current_op_code_global),
            GlobalGet(open_op_codes_global),
            I32Const(1),
            I32Sub,
            GlobalSet(open_op_codes_global),
            End,
        ] {
            if matches!(instruction, Call(index) if index == weight_consumed_function_index) {
                function.instruction(&LocalSet(4));
                for index in 5..5 + getter_locals {
                    function.instruction(&I32Const(0));
                    function.instruction(&LocalSet(index));
                }
                for operator in getter.get_operators_reader()? {
                    let instruction = match operator? {
                        WasmOperator::LocalGet { local_index: 0 } => {
                            Block(encoder::BlockType::Result(encoder::ValType::I64))
                        }
                        WasmOperator::I64Store { memarg } => {
                            function.instruction(&End);
                            LocalSet(if memarg.offset == 0 {
                                ref_time_local
                            } else {
                                proof_size_local
                            })
                        }
                        WasmOperator::LocalGet { local_index } => LocalGet(local_index + 3),
                        WasmOperator::LocalSet { local_index } => LocalSet(local_index + 3),
                        WasmOperator::LocalTee { local_index } => LocalTee(local_index + 3),
                        WasmOperator::End => continue,
                        operator => self.instruction(operator)?,
                    };
                    function.instruction(&instruction);
                }
            } else {
                function.instruction(&instruction);
            }
        }
        Ok(function)
    }

    fn call_frame_wrapper(&self) -> encoder::Function {
        let mut function = encoder::Function::new([(5, encoder::ValType::I32)]);
        let layout = &self.metadata.call_frame_layout;
        let current_op_code_global = self.metadata.global_count;
        let open_op_codes_global = self.metadata.global_count + 1;
        // The supported wasm32 ABI passes input_data as a Vec pointer:
        // capacity at byte 0, data pointer at byte 4, length at byte 8.
        let input_data_pointer = MemArg {
            offset: 4,
            align: 2,
            memory_index: 0,
        };
        let input_data_length = MemArg {
            offset: 8,
            ..input_data_pointer
        };
        let selector_bytes = MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        };
        for instruction in [
            GlobalGet(current_op_code_global),
            LocalSet(4),
            GlobalGet(open_op_codes_global),
            LocalSet(5),
            LocalGet(5),
            If(encoder::BlockType::Empty),
            LocalGet(3),
            I32Load(input_data_length),
            LocalSet(6),
            LocalGet(6),
            I32Const(4),
            I32GeU,
            If(encoder::BlockType::Empty),
            LocalGet(3),
            I32Load(input_data_pointer),
            I32Load(selector_bytes),
            LocalSet(7),
            End,
            LocalGet(4),
            LocalGet(7),
            LocalGet(6),
            LocalGet(1),
            I32Load(MemArg {
                offset: layout.frames_pointer_offset,
                ..input_data_pointer
            }),
            LocalGet(1),
            I32Load(MemArg {
                offset: layout.frames_length_offset,
                ..input_data_pointer
            }),
            LocalTee(8),
            I32Const(layout.frame_size),
            I32Mul,
            I32Add,
            I32Const(-layout.frame_size),
            I32Add,
            LocalGet(1),
            LocalGet(8),
            Select,
            I32Const(layout.code_address_offset),
            I32Add,
            Call(self.metadata.imported_function_count + 2),
            End,
            LocalGet(0),
            LocalGet(1),
            LocalGet(2),
            LocalGet(3),
            Call(self.original_index(self.metadata.run_frame_function_index)),
            LocalGet(5),
            If(encoder::BlockType::Empty),
            LocalGet(4),
            LocalGet(7),
            LocalGet(6),
            Call(self.metadata.imported_function_count + 3),
            End,
            End,
        ] {
            function.instruction(&instruction);
        }
        function
    }
}

impl Reencode for Instrumenter<'_> {
    type Error = Infallible;

    fn function_index(
        &mut self,
        index: u32,
    ) -> std::result::Result<u32, reencode::Error<Self::Error>> {
        Ok(if index == self.metadata.exec_instruction_function_index {
            self.metadata.function_count + HOST_FUNCTION_COUNT
        } else if index == self.metadata.run_frame_function_index {
            self.metadata.function_count + HOST_FUNCTION_COUNT + 1
        } else if index == self.metadata.transaction_result_function_index {
            self.metadata.function_count + HOST_FUNCTION_COUNT + 2
        } else {
            self.original_index(index)
        })
    }

    fn parse_type_section(
        &mut self,
        types: &mut encoder::TypeSection,
        section: parser::TypeSectionReader<'_>,
    ) -> std::result::Result<(), reencode::Error<Self::Error>> {
        reencode::utils::parse_type_section(self, types, section)?;
        types.ty().function(
            [
                encoder::ValType::I32,
                encoder::ValType::I64,
                encoder::ValType::I64,
            ],
            [],
        );
        types.ty().function([encoder::ValType::I32; 3], []);
        types.ty().function([encoder::ValType::I32], []);
        types.ty().function([encoder::ValType::I32; 4], []);
        Ok(())
    }

    fn parse_import_section(
        &mut self,
        imports: &mut encoder::ImportSection,
        section: parser::ImportSectionReader<'_>,
    ) -> std::result::Result<(), reencode::Error<Self::Error>> {
        reencode::utils::parse_import_section(self, imports, section)?;
        self.add_imports(imports);
        Ok(())
    }

    fn intersperse_section_hook(
        &mut self,
        module: &mut encoder::Module,
        _after: Option<encoder::SectionId>,
        before: Option<encoder::SectionId>,
    ) -> std::result::Result<(), reencode::Error<Self::Error>> {
        if !self.metadata.has_import_section && before == Some(encoder::SectionId::Function) {
            let mut imports = encoder::ImportSection::new();
            self.add_imports(&mut imports);
            module.section(&imports);
        }
        Ok(())
    }

    fn parse_function_section(
        &mut self,
        functions: &mut encoder::FunctionSection,
        section: parser::FunctionSectionReader<'_>,
    ) -> std::result::Result<(), reencode::Error<Self::Error>> {
        reencode::utils::parse_function_section(self, functions, section)?;
        functions.function(self.metadata.exec_instruction_type_index);
        functions.function(self.metadata.run_frame_type_index);
        functions.function(self.metadata.transaction_result_type_index);
        Ok(())
    }

    fn parse_global_section(
        &mut self,
        globals: &mut encoder::GlobalSection,
        section: parser::GlobalSectionReader<'_>,
    ) -> std::result::Result<(), reencode::Error<Self::Error>> {
        reencode::utils::parse_global_section(self, globals, section)?;
        let counter = encoder::GlobalType {
            val_type: encoder::ValType::I32,
            mutable: true,
            shared: false,
        };
        globals.global(counter, &encoder::ConstExpr::i32_const(0));
        globals.global(counter, &encoder::ConstExpr::i32_const(0));
        Ok(())
    }

    fn parse_code_section(
        &mut self,
        code: &mut encoder::CodeSection,
        section: parser::CodeSectionReader<'_>,
    ) -> std::result::Result<(), reencode::Error<Self::Error>> {
        reencode::utils::parse_code_section(self, code, section)?;
        code.function(&self.opcode_wrapper()?);
        code.function(&self.call_frame_wrapper());
        code.function(&self.transaction_result_wrapper());
        Ok(())
    }
}
