(module
  (type $instruction (func (param i32 i32 i32)))
  (import "env" "ext_allocator_malloc_version_1" (func $malloc (param i32) (result i32)))
  (memory (export "memory") 2)
  (global (export "__heap_base") i32 (i32.const 4096))
  (global $__stack_pointer (mut i32) (i32.const 2048))
  (table 1 funcref)
  (elem (i32.const 0) $pallet_revive::vm::evm::instructions::exec_instruction)
  (data (i32.const 128) "\04\00\00\00\90\00\00\00\04\00\00\00")
  (data (i32.const 144) "\12\34\56\78")
  (data (i32.const 192) "\00\01\00\00\01\00\00\00")
  (data (i32.const 272) "\22\22\22\22\22\22\22\22\22\22\22\22\22\22\22\22\22\22\22\22")
  (data (i32.const 336) "\33\33\33\33\33\33\33\33\33\33\33\33\33\33\33\33\33\33\33\33")
  (func $memcmp (param i32 i32 i32) (result i32) i32.const 0)
  (func $_ZN116_$LT$pallet_revive..vm..evm..interpreter..Interpreter$LT$E$GT$$u20$as$u20$pallet_revive..tracing..FrameTraceInfo$GT$15weight_consumed17h5e8e4e80cc9f7841E
    (param $output i32) (param $interpreter i32)
    local.get $output
    local.get $interpreter
    i64.load
    i64.store
    local.get $output
    local.get $interpreter
    i64.load offset=8
    i64.store offset=8
  )
  (func $charge (param $interpreter i32) (param $weight i64)
    local.get $interpreter
    local.get $interpreter
    i64.load
    local.get $weight
    i64.add
    i64.store
  )
  (func $pallet_revive::vm::evm::instructions::exec_instruction (type $instruction)
    (param $result i32) (param $interpreter i32) (param $opcode i32)
    local.get $opcode
    i32.eqz
    if
      return
    end
    local.get $opcode
    i32.const 255
    i32.eq
    if
      unreachable
    end
    local.get $opcode
    i32.const 241
    i32.eq
    if
      local.get $interpreter
      i64.const 5
      call $charge
      local.get $result
      i32.const 64
      i32.const 1
      i32.const 128
      call $pallet_revive::exec::Stack::run
      local.get $interpreter
      i64.const 10
      call $charge
      return
    end
    local.get $opcode
    i32.const 240
    i32.eq
    if
      local.get $result
      i32.const 320
      i32.const 1
      i32.const 160
      call $pallet_revive::exec::Stack::run
      return
    end
    local.get $opcode
    i32.const 250
    i32.eq
    if
      local.get $result
      i32.const 64
      i32.const 2
      i32.const 128
      call $pallet_revive::exec::Stack::run
      return
    end
    local.get $opcode
    i32.const 85
    i32.eq
    if
      local.get $interpreter
      i64.const 100
      call $charge
      local.get $interpreter
      i64.const -98
      call $charge
      return
    end
    local.get $opcode
    i32.const 254
    i32.eq
    if
      local.get $interpreter
      i64.const -2
      call $charge
      return
    end
    local.get $interpreter
    i64.const 7
    call $charge
  )
  (func $pallet_revive::exec::Stack::run
    (param $result i32) (param $stack i32) (param $executable i32) (param $input i32)
    (local $frame_count i32) (local $frame i32) (local $address i32)
    local.get $stack
    i32.load offset=128
    local.get $stack
    i32.load offset=132
    local.tee $frame_count
    i32.const 48
    i32.mul
    i32.add
    i32.const -48
    i32.add
    local.get $stack
    local.get $frame_count
    select
    local.tee $frame
    drop
    local.get $frame
    i32.const 16
    i32.add
    local.tee $address
    local.get $stack
    i32.const 16
    i32.add
    i32.const 20
    call $memcmp
    drop
    local.get $executable
    i32.const 1
    i32.eq
    if
      local.get $result
      local.get $stack
      i32.const 1
      call $pallet_revive::vm::evm::instructions::exec_instruction
      return
    end
    local.get $executable
    i32.const 2
    i32.eq
    if
      return
    end
    i32.const 0
    i32.const 32
    i32.const 241
    i32.const 0
    call_indirect (type $instruction)
    i32.const 0
    i32.const 32
    i32.const 85
    call $pallet_revive::vm::evm::instructions::exec_instruction
    i32.const 0
    i32.const 32
    i32.const 0
    call $pallet_revive::vm::evm::instructions::exec_instruction
  )
  (func (export "execute") (param i32 i32) (result i64)
    i32.const 8
    call $malloc
    drop
    i32.const 0
    i32.const 32
    i32.const 0
    i32.const 160
    call $pallet_revive::exec::Stack::run
    i64.const 68719476768
  )
  (func (export "create") (param i32 i32) (result i64)
    i32.const 0
    i32.const 32
    i32.const 240
    call $pallet_revive::vm::evm::instructions::exec_instruction
    i64.const 0
  )
  (func (export "precompile") (param i32 i32) (result i64)
    i32.const 0
    i32.const 32
    i32.const 250
    call $pallet_revive::vm::evm::instructions::exec_instruction
    i64.const 0
  )
  (func (export "no_frame") (param i32 i32) (result i64)
    i32.const 0
    i32.const 32
    i32.const 244
    call $pallet_revive::vm::evm::instructions::exec_instruction
    i64.const 0
  )
  (func (export "trap") (param i32 i32) (result i64)
    i32.const 0
    i32.const 32
    i32.const 255
    call $pallet_revive::vm::evm::instructions::exec_instruction
    i64.const 0
  )
  (func (export "decreasing_weight") (param i32 i32) (result i64)
    i32.const 32
    i64.const 100
    call $charge
    i32.const 0
    i32.const 32
    i32.const 254
    call $pallet_revive::vm::evm::instructions::exec_instruction
    i64.const 0
  )

  (func $pallet_revive::evm::block_storage::EthereumCallResult::new
    (param i32 i32 i32 i64 i64 i32 i32 i32)
    local.get 2 i32.load8_u offset=112 i32.const 15 i32.ne drop
    local.get 2 i32.load offset=116 drop
    local.get 2 i32.load offset=120 drop
    local.get 2 i32.load offset=124 drop
    local.get 2 i32.load8_u offset=128 i32.const 1 i32.and drop
  )

  (func (export "transaction") (param i32 i32) (result i64)
    i32.const 512 i32.const 15 i32.store8 offset=112
    i32.const 512 i32.const 4 i32.store offset=116
    i32.const 512 i32.const 144 i32.store offset=120
    i32.const 512 i32.const 4 i32.store offset=124
    i32.const 512 i32.const 0 i32.store offset=128
    i32.const 0 i32.const 0 i32.const 512 i64.const 0 i64.const 0
    i32.const 0 i32.const 0 i32.const 0
    call $pallet_revive::evm::block_storage::EthereumCallResult::new
    i64.const 0
  )
  (func (export "reverted_transaction") (param i32 i32) (result i64)
    i32.const 512 i32.const 15 i32.store8 offset=112
    i32.const 512 i32.const 4 i32.store offset=116
    i32.const 512 i32.const 144 i32.store offset=120
    i32.const 512 i32.const 4 i32.store offset=124
    i32.const 512 i32.const 1 i32.store offset=128
    i32.const 0 i32.const 0 i32.const 512 i64.const 0 i64.const 0
    i32.const 0 i32.const 0 i32.const 0
    call $pallet_revive::evm::block_storage::EthereumCallResult::new
    i64.const 0
  )
)
