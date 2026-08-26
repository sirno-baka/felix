

pub const SYS_EXECVE_WASM: u32 = 1000;

use crate::syscalls::handler::*;
use wasmi::{Engine, Module, Store, Linker, Instance, Memory, MemoryType, Value, Extern, Func, Caller};
use wasmi::core::HostError;
use alloc::boxed::Box;
use interrupt_sync::without_interrupts;
use crate::filesystem::file::{FileDescriptor, FileDescriptorTable};
use crate::memory::paging::{copy_kernel_mappings, PDEFlags};
use crate::multitasking::task::{CPUState, Task, TASK_MANAGER};
use crate::println;
use crate::wrappers::{cli, hlt, sti};

/// Сигнатура хостовых функций для WASI
type WasmHostResult = Result<u32, Box<dyn HostError>>;



/// Регистрация базовых WASI-функций в линкере
/// Регистрация базовых WASI-функций в линкере
fn register_wasi_functions(linker: &mut Linker<()>, _ctx: &WasmTaskContext) -> Result<(), wasmi::Error> {
    // proc_exit
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "proc_exit",
        |_code: i32| -> Result<(), wasmi::core::Trap> {
            unsafe {
                let slot = TASK_MANAGER.get_current_slot() as usize;
                sys_exit(slot, 0);
            }
            Err(wasmi::core::Trap::new("proc_exit called"))
        },
    )?;

    // fd_write (только для stdout/stderr)
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "fd_write",
        |mut caller: wasmi::Caller<'_, ()>, fd: i32, iovs_ptr: i32, iovs_len: i32, nwritten_ptr: i32| -> Result<u32, wasmi::core::Trap> {
            without_interrupts(|| {


            if iovs_ptr < 0 || iovs_len < 0 {
                return Err(wasmi::core::Trap::new("invalid iovs"));
            }
            let iovs_start = iovs_ptr as usize;
            let iovs_len_usize = iovs_len as usize;
            let iovs_end = iovs_start.saturating_add(iovs_len_usize.saturating_mul(8));

            let memory = caller
                .get_export("memory")
                .and_then(|ext| ext.into_memory())
                .ok_or_else(|| wasmi::core::Trap::new("memory not found"))?;

            let data = memory.data(&caller);
            if iovs_end > data.len() {
                return Err(wasmi::core::Trap::new("iovs out of bounds"));
            }

            let mut total = 0usize;
            for i in 0..iovs_len_usize {
                let base = iovs_start + i * 8;
                let buf_ptr = u32::from_le_bytes(data[base..base + 4].try_into().unwrap()) as usize;
                let buf_len = u32::from_le_bytes(data[base + 4..base + 8].try_into().unwrap()) as usize;

                if buf_ptr + buf_len > data.len() {
                    continue;
                }
                let buf_slice = &data[buf_ptr..buf_ptr + buf_len];

                let current_slot = unsafe { TASK_MANAGER.get_current_slot() as usize };
                let written = unsafe { sys_write(current_slot, fd as usize, buf_slice.as_ptr(), buf_slice.len()) };
                total += written;
            }

            let nwritten_addr = nwritten_ptr as usize;
            if nwritten_addr + 4 <= data.len() {
                memory
                    .write(&mut caller, nwritten_addr, &(total as u32).to_le_bytes())
                    .map_err(|_| wasmi::core::Trap::new("write nwritten failed"))?;
            }

            Ok(total as u32)
            })
        },
    )?;

    // fd_close
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "fd_close",
        |_fd: i32| -> Result<(), wasmi::core::Trap> {
            // Пока просто возвращаем успех, можно добавить реальный sys_close
            Ok(())
        },
    )?;

    // === ДОБАВЛЕНЫ ЗАГЛУШКИ ДЛЯ WASI ===

    // environ_sizes_get: сообщает размер и количество переменных окружения
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "environ_sizes_get",
        |mut caller: wasmi::Caller<'_, ()>, environ_count_ptr: i32, environ_buf_size_ptr: i32| -> Result<i32, wasmi::core::Trap> {
            let memory = caller
                .get_export("memory")
                .and_then(|ext| ext.into_memory())
                .ok_or_else(|| wasmi::core::Trap::new("memory not found"))?;

            // 0 переменных окружения, 0 байт
            memory.write(&mut caller, environ_count_ptr as usize, &0u32.to_le_bytes())
                .map_err(|_| wasmi::core::Trap::new("write environ_count failed"))?;
            memory.write(&mut caller, environ_buf_size_ptr as usize, &0u32.to_le_bytes())
                .map_err(|_| wasmi::core::Trap::new("write environ_buf_size failed"))?;

            Ok(0) // WASI_ESUCCESS
        },
    )?;

    // environ_get: получает сами переменные окружения
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "environ_get",
        |_caller: wasmi::Caller<'_, ()>, _environ_ptrs_ptr: i32, _environ_buf_ptr: i32| -> Result<i32, wasmi::core::Trap> {
            // Так как переменных 0, писать нечего, просто возвращаем успех
            Ok(0) // WASI_ESUCCESS
        },
    )?;

    // args_sizes_get: сообщает размер и количество аргументов командной строки
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "args_sizes_get",
        |mut caller: wasmi::Caller<'_, ()>, argc_ptr: i32, argv_buf_size_ptr: i32| -> Result<i32, wasmi::core::Trap> {
            let memory = caller
                .get_export("memory")
                .and_then(|ext| ext.into_memory())
                .ok_or_else(|| wasmi::core::Trap::new("memory not found"))?;

            // 0 аргументов, 0 байт
            memory.write(&mut caller, argc_ptr as usize, &0u32.to_le_bytes())
                .map_err(|_| wasmi::core::Trap::new("write argc failed"))?;
            memory.write(&mut caller, argv_buf_size_ptr as usize, &0u32.to_le_bytes())
                .map_err(|_| wasmi::core::Trap::new("write argv_buf_size failed"))?;

            Ok(0) // WASI_ESUCCESS
        },
    )?;

    // args_get: получает сами аргументы командной строки
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "args_get",
        |_caller: wasmi::Caller<'_, ()>, _argv_ptrs_ptr: i32, _argv_buf_ptr: i32| -> Result<i32, wasmi::core::Trap> {
            // Так как аргументов 0, писать нечего, просто возвращаем успех
            Ok(0) // WASI_ESUCCESS
        },
    )?;

    Ok(())
}

/// Контекст для WASI
struct WasmTaskContext { slot: usize }

/// Структура для хранения состояния WASM-машины задачи
struct WasmExec {
    store: Store<()>,
    instance: Instance,
}

// Глобальное хранилище (в идеале должно быть полем в Task)
static mut WASM_EXEC: [Option<Box<WasmExec>>; 8] = {
    const NONE: Option<Box<WasmExec>> = None;
    [NONE; 8]
};


/// Реализация sys_execve_wasm
pub(crate) fn sys_execve_wasm(
    parent_slot: usize,
    buf_ptr: *const u8,
    count: usize,
    params_ptr: *const ExecParamsUser,
) -> usize {
    let bytecode = unsafe { core::slice::from_raw_parts(buf_ptr, count) };
    if bytecode.is_empty() { return usize::MAX; }

    let params = read_exec_params(params_ptr);
    let slot_i8 = unsafe { TASK_MANAGER.get_free_slot() };
    if slot_i8 < 0 { return usize::MAX; }
    let slot = slot_i8 as usize;

    let engine = Engine::default();
    let module = match Module::new(&engine, bytecode) {
        Ok(m) => m,
        Err(_) => return usize::MAX,
    };

    unsafe {
        cli!();
        let mut task = Task::new_task();

        // 1. КРИТИЧНО: Копируем маппинги ядра, иначе Triple Fault при прерывании!
        let pd_phys = task.page_dir_phys;
        copy_kernel_mappings(task.pd_mut(), pd_phys);
        // Self-mapping для page directory
        task.pd_mut().entries[1023] = pd_phys | PDEFlags::PRESENT | PDEFlags::WRITABLE;

        let kernel_stack_top = task.stack_base + crate::multitasking::task::STACK_SIZE as u32;
        task.kernel_stack = kernel_stack_top;
        let state_ptr = (kernel_stack_top as usize
            - crate::multitasking::task::HEADROOM
            - core::mem::size_of::<CPUState>())
            as *mut CPUState;
        task.cpu_state_ptr = state_ptr as u32;

        // 2. Настраиваем WASM
        let mut linker = Linker::new(&engine);
        let ctx = WasmTaskContext { slot };
        register_wasi_functions(&mut linker, &ctx).unwrap();
        let mut store = Store::new(&engine, ());
        let memory_type = MemoryType::new(1, Some(32)).unwrap();
        let memory = Memory::new(&mut store, memory_type).unwrap();
        linker.define("env", "memory", Extern::Memory(memory.clone())).unwrap();
        let instance = linker.instantiate(&mut store, &module).unwrap().start(&mut store).unwrap();

        // 3. Сохраняем состояние в глобальный массив
        WASM_EXEC[slot] = Some(Box::new(WasmExec { store, instance }));

        // 4. Настраиваем CPUState для KERNEL MODE (Ring 0)!
        *state_ptr = CPUState {
            eax: 0, ebx: 0, ecx: 0, edx: 0,
            esi: 0, edi: 0, ebp: 0,
            eip: wasm_task_entry as u32,
            cs: 0x08,      // Kernel Code Segment (не 0x1B!)
            eflags: 0x202,
            esp: kernel_stack_top - 12,
            ss: 0x10,      // Kernel Data Segment (не 0x23!)
        };

        task.running = true;
        task.parent = parent_slot as i8;
        task.zombie = false;
        task.exit_code = 0;

        let mut fd_table = FileDescriptorTable::with_stdio();
        install_child_fd(parent_slot, &mut fd_table, 0, params.stdin, FileDescriptor::ConsoleIn);
        install_child_fd(parent_slot, &mut fd_table, 1, params.stdout, FileDescriptor::ConsoleOut);
        install_child_fd(parent_slot, &mut fd_table, 2, params.stderr, FileDescriptor::ConsoleOut);
        task.fd_table = fd_table;

        TASK_MANAGER.tasks[slot] = Some(task);
        TASK_MANAGER.task_count += 1;
        sti!();
        slot
    }
}


/// Точка входа для Wasm-задачи
#[no_mangle]
extern "C" fn wasm_task_entry() -> ! {
    let slot = unsafe { TASK_MANAGER.get_current_slot() as usize };

    // Забираем WASM-состояние из глобального массива
    let exec = unsafe { WASM_EXEC[slot].take() };

    if let Some(mut exec) = exec {
        // Пытаемся найти и вызвать точку входа (_start или main)
        if let Some(start_func) = exec.instance.get_func(&mut exec.store, "_start") {
            let _ = start_func.call(&mut exec.store, &[], &mut []);
        } else if let Some(main_func) = exec.instance.get_func(&mut exec.store, "main") {
            let _ = main_func.call(&mut exec.store, &[], &mut []);
        }
    }

    // Завершаем задачу
    unsafe {
        sys_exit(slot, 0);
    }

    loop {
        unsafe { hlt!() }
    }
}