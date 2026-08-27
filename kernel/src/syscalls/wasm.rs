pub const SYS_EXECVE_WASM: u32 = 1000;

use crate::filesystem::file::{FileDescriptor, FileDescriptorTable};
use crate::memory::paging::{PDEFlags, copy_kernel_mappings};
use crate::multitasking::task::{CPUState, TASK_MANAGER, Task};
use crate::print::klog_write_str;
use crate::syscalls::handler::*;
use crate::wrappers::{cli, hlt, sti};
use crate::{print, println};
use alloc::boxed::Box;
use core::str::Utf8Error;
use interrupt_sync::without_interrupts;
use wasmi::core::HostError;
use wasmi::{
    Caller, Engine, Error, Extern, Func, Instance, Linker, Memory, MemoryType, Module, Store, Value,
};

/// Сигнатура хостовых функций для WASI
type WasmHostResult = Result<u32, Box<dyn HostError>>;

/// Регистрация базовых WASI-функций в линкере
/// Регистрация базовых WASI-функций в линкере
fn register_wasi_functions(
    linker: &mut Linker<()>,
    _ctx: &WasmTaskContext,
) -> Result<(), wasmi::Error> {
    // proc_exit
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "proc_exit",
        |_code: i32| -> Result<(), wasmi::core::Trap> {
            without_interrupts(|| {
                unsafe {
                    let slot = TASK_MANAGER.get_current_slot() as usize;
                    println!("proc exit {}", slot);
                    sys_exit(slot, 0);
                }
                Err(wasmi::core::Trap::new("proc_exit called"))
            })
        },
    )?;

    // sock_open (создание сокета, необходимо для TcpStream::connect)
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "sock_open",
        |mut caller: wasmi::Caller<'_, ()>,
         af: i32,
         socktype: i32,
         protocol: i32,
         fd_ptr: i32|
         -> Result<u32, wasmi::core::Trap> {
            println!("socket open");
            without_interrupts(|| {
                let current_slot = unsafe { TASK_MANAGER.get_current_slot() as usize };
                // sys_socket возвращает fd при успехе или usize::MAX при ошибке
                let res =
                    unsafe { sys_socket(current_slot, af as u16, socktype as u16, protocol as u8) };

                if res == usize::MAX {
                    return Ok(8); // WASI_EBADF
                }

                let memory = caller
                    .get_export("memory")
                    .and_then(|ext| ext.into_memory())
                    .ok_or_else(|| wasmi::core::Trap::new("memory not found"))?;
                memory
                    .write(&mut caller, fd_ptr as usize, &(res as u32).to_le_bytes())
                    .map_err(|_| wasmi::core::Trap::new("write fd failed"))?;

                Ok(0) // WASI_ESUCCESS
            })
        },
    )?;

    // sock_connect (подключение сокета)
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "sock_connect",
        |mut caller: wasmi::Caller<'_, ()>,
         fd: i32,
         addr_ptr: i32,
         addr_len: i32|
         -> Result<u32, wasmi::core::Trap> {
            println!("sock_connect");
            without_interrupts(|| {
                let current_slot = unsafe { TASK_MANAGER.get_current_slot() as usize };
                let memory = caller
                    .get_export("memory")
                    .and_then(|ext| ext.into_memory())
                    .ok_or_else(|| wasmi::core::Trap::new("memory not found"))?;
                let data = memory.data(&caller);

                if addr_ptr < 0
                    || addr_len < 0
                    || (addr_ptr as usize).saturating_add(addr_len as usize) > data.len()
                {
                    return Ok(14); // WASI_EFAULT
                }

                let res = unsafe {
                    sys_connect(
                        current_slot,
                        fd as usize,
                        data.as_ptr().add(addr_ptr as usize),
                        addr_len as usize,
                    )
                };

                if res == 0 {
                    Ok(0) // WASI_ESUCCESS
                } else {
                    Ok(8) // WASI_EBADF или другая ошибка
                }
            })
        },
    )?;

    // === Заглушки, которые часто вызывает std::net ===

    // sock_setopt (настройка таймаутов и т.д.)
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "sock_setopt",
        |_fd: i32,
         _level: i32,
         _option: i32,
         _optval_ptr: i32,
         _optval_len: i32|
         -> Result<u32, wasmi::core::Trap> {
            Ok(0) // WASI_ESUCCESS
        },
    )?;

    // sock_getopt
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "sock_getopt",
        |_fd: i32,
         _level: i32,
         _option: i32,
         _optval_ptr: i32,
         _optval_len_ptr: i32|
         -> Result<u32, wasmi::core::Trap> {
            Ok(0) // WASI_ESUCCESS
        },
    )?;

    // fd_fdstat_set_flags (установка O_NONBLOCK, часто вызывается перед connect)
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "fd_fdstat_set_flags",
        |_fd: i32, _flags: i32| -> Result<u32, wasmi::core::Trap> {
            Ok(0) // WASI_ESUCCESS
        },
    )?;
    // fd_read
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "fd_read",
        |mut caller: wasmi::Caller<'_, ()>,
         fd: i32,
         iovs_ptr: i32,
         iovs_len: i32,
         nread_ptr: i32|
         -> Result<u32, wasmi::core::Trap> {
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
                let current_slot = unsafe { TASK_MANAGER.get_current_slot() as usize };
                let mem_base_ptr = data.as_ptr() as *mut u8;

                for i in 0..iovs_len_usize {
                    let base = iovs_start + i * 8;
                    let buf_ptr =
                        u32::from_le_bytes(data[base..base + 4].try_into().unwrap()) as usize;
                    let buf_len =
                        u32::from_le_bytes(data[base + 4..base + 8].try_into().unwrap()) as usize;

                    // Проверка выхода за границы памяти WASM
                    if buf_ptr
                        .checked_add(buf_len)
                        .map_or(true, |end| end > data.len())
                    {
                        return Err(wasmi::core::Trap::new("buffer out of bounds"));
                    }

                    // Получаем мутабельный слайс прямо в памяти WASM
                    let buf_slice = unsafe {
                        core::slice::from_raw_parts_mut(mem_base_ptr.add(buf_ptr), buf_len)
                    };
                    println!("sys_read {}", fd);
                    // Вызываем sys_read из handler.rs
                    let read = unsafe {
                        sys_read(
                            current_slot,
                            fd as usize,
                            buf_slice.as_mut_ptr(),
                            buf_slice.len(),
                        )
                    };

                    total += read;
                    if read < buf_len {
                        break; // EOF или больше нет данных для чтения
                    }
                }

                let nread_addr = nread_ptr as usize;
                if nread_addr
                    .checked_add(4)
                    .map_or(true, |end| end > data.len())
                {
                    return Err(wasmi::core::Trap::new("nread out of bounds"));
                }

                // Записываем количество прочитанных байт обратно в память WASM
                memory
                    .write(&mut caller, nread_addr, &(total as u32).to_le_bytes())
                    .map_err(|_| wasmi::core::Trap::new("write nread failed"))?;

                Ok(0) // WASI_ESUCCESS
            })
        },
    )?;
    // fd_write (только для stdout/stderr)
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "fd_write",
        |mut caller: wasmi::Caller<'_, ()>,
         fd: i32,
         iovs_ptr: i32,
         iovs_len: i32,
         nwritten_ptr: i32|
         -> Result<u32, wasmi::core::Trap> {
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
                    let buf_ptr =
                        u32::from_le_bytes(data[base..base + 4].try_into().unwrap()) as usize;
                    let buf_len =
                        u32::from_le_bytes(data[base + 4..base + 8].try_into().unwrap()) as usize;

                    if buf_ptr + buf_len > data.len() {
                        continue;
                    }
                    let buf_slice = &data[buf_ptr..buf_ptr + buf_len];

                    let current_slot = unsafe { TASK_MANAGER.get_current_slot() as usize };
                    match core::str::from_utf8(buf_slice) {
                        Ok(v) => {
                            print!("{}", v);
                        }
                        _ => {}
                    }

                    println!("sys_write {}", fd);
                    let written = unsafe {
                        sys_write(
                            current_slot,
                            fd as usize,
                            buf_slice.as_ptr(),
                            buf_slice.len(),
                        )
                    };
                    total += written;
                }

                let nwritten_addr = nwritten_ptr as usize;
                if nwritten_addr + 4 <= data.len() {
                    memory
                        .write(&mut caller, nwritten_addr, &(total as u32).to_le_bytes())
                        .map_err(|_| wasmi::core::Trap::new("write nwritten failed"))?;
                }
                println!("written n={}", total);
                Ok(0)
            })
        },
    )?;

    // fd_fdstat_get
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "fd_fdstat_get",
        |mut caller: wasmi::Caller<'_, ()>,
         _fd: i32,
         stat_ptr: i32|
         -> Result<u32, wasmi::core::Trap> {
            println!("fd_fdstat_get");
            let memory = caller
                .get_export("memory")
                .and_then(|e| e.into_memory())
                .ok_or_else(|| wasmi::core::Trap::new("no mem"))?;
            // нули
            let zeros = [0u8; 24];
            memory
                .write(&mut caller, stat_ptr as usize, &zeros)
                .map_err(|_| wasmi::core::Trap::new("write"))?;
            Ok(0)
        },
    )?;

    // fd_fdstat_get
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "poll_oneoff",
        |mut caller: wasmi::Caller<'_, ()>,
         _fd: i32,
         stat_ptr: i32|
         -> Result<u32, wasmi::core::Trap> {
            println!("poll_oneoff");
            Ok(0)
        },
    )?;

    // fd_close
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "fd_close",
        |fd: i32| -> Result<u32, wasmi::core::Trap> {
            without_interrupts(|| {
                let current_slot = unsafe { TASK_MANAGER.get_current_slot() as usize };
                let res = unsafe { sys_close(current_slot, fd as usize) };
                println!("sys_close {}", fd);
                // sys_close возвращает 0 при успехе и usize::MAX при ошибке
                if res == 0 {
                    Ok(0) // WASI_ESUCCESS
                } else {
                    Ok(8) // WASI_EBADF (Bad file descriptor)
                }
            })
        },
    )?;

    // === ДОБАВЛЕНЫ ЗАГЛУШКИ ДЛЯ WASI ===
    // environ_sizes_get
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "environ_sizes_get",
        |mut caller: wasmi::Caller<'_, ()>,
         count_ptr: i32,
         buf_size_ptr: i32|
         -> Result<i32, wasmi::core::Trap> {
            let memory = caller
                .get_export("memory")
                .and_then(|e| e.into_memory())
                .ok_or_else(|| wasmi::core::Trap::new("no mem"))?;
            // 1 var, size of "RUST_BACKTRACE=1\0"
            memory
                .write(&mut caller, count_ptr as usize, &1u32.to_le_bytes())
                .map_err(|_| wasmi::core::Trap::new("write"))?;
            memory
                .write(&mut caller, buf_size_ptr as usize, &17u32.to_le_bytes())
                .map_err(|_| wasmi::core::Trap::new("write"))?;
            Ok(0)
        },
    )?;

    // environ_get
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "environ_get",
        |mut caller: wasmi::Caller<'_, ()>,
         environ_ptrs: i32,
         environ_buf: i32|
         -> Result<i32, wasmi::core::Trap> {
            let memory = caller
                .get_export("memory")
                .and_then(|e| e.into_memory())
                .ok_or_else(|| wasmi::core::Trap::new("no mem"))?;
            let s = b"RUST_BACKTRACE=1\0";
            memory
                .write(&mut caller, environ_buf as usize, s)
                .map_err(|_| wasmi::core::Trap::new("write"))?;
            memory
                .write(
                    &mut caller,
                    environ_ptrs as usize,
                    &(environ_buf as u32).to_le_bytes(),
                )
                .map_err(|_| wasmi::core::Trap::new("write"))?;
            Ok(0)
        },
    )?;

    // args_sizes_get
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "args_sizes_get",
        |mut caller: wasmi::Caller<'_, ()>,
         argc_ptr: i32,
         argv_buf_size_ptr: i32|
         -> Result<i32, wasmi::core::Trap> {
            let memory = caller
                .get_export("memory")
                .and_then(|e| e.into_memory())
                .ok_or_else(|| wasmi::core::Trap::new("memory not found"))?;
            // 1 arg, size of "http-client\0"
            memory
                .write(&mut caller, argc_ptr as usize, &1u32.to_le_bytes())
                .map_err(|_| wasmi::core::Trap::new("write argc failed"))?;
            memory
                .write(&mut caller, argv_buf_size_ptr as usize, &6u32.to_le_bytes())
                .map_err(|_| wasmi::core::Trap::new("write size failed"))?;
            Ok(0)
        },
    )?;

    // args_get
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "args_get",
        |mut caller: wasmi::Caller<'_, ()>,
         argv_ptrs_ptr: i32,
         argv_buf_ptr: i32|
         -> Result<i32, wasmi::core::Trap> {
            let memory = caller
                .get_export("memory")
                .and_then(|e| e.into_memory())
                .ok_or_else(|| wasmi::core::Trap::new("memory not found"))?;
            let buf = b"http-client\0";

            memory
                .write(&mut caller, argv_buf_ptr as usize, buf)
                .map_err(|_| wasmi::core::Trap::new("write argc failed"))?;
            memory
                .write(
                    &mut caller,
                    argv_ptrs_ptr as usize,
                    &(argv_buf_ptr as u32).to_le_bytes(),
                )
                .map_err(|_| wasmi::core::Trap::new("write argc failed"))?;
            Ok(0)
        },
    )?;

    // === WASI Sockets ===

    // sock_shutdown
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "sock_shutdown",
        |fd: i32, how: i32| -> Result<u32, wasmi::core::Trap> {
            without_interrupts(|| {
                let current_slot = unsafe { TASK_MANAGER.get_current_slot() as usize };
                let res = unsafe { sys_shutdown(current_slot, fd as usize, how as u32) };
                println!("sys_shutdown {}", fd);
                Ok(if res == 0 { 0 } else { 8 }) // 0 = ESUCCESS, 8 = EBADF
            })
        },
    )?;

    // sock_recv
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "sock_recv",
        |mut caller: wasmi::Caller<'_, ()>,
         fd: i32,
         ri_data_ptr: i32,
         ri_data_len: i32,
         _ri_flags: i32,
         ro_datalen_ptr: i32,
         ro_flags_ptr: i32|
         -> Result<u32, wasmi::core::Trap> {
            without_interrupts(|| {
                let memory = caller
                    .get_export("memory")
                    .and_then(|ext| ext.into_memory())
                    .ok_or_else(|| wasmi::core::Trap::new("memory not found"))?;
                let data = memory.data(&caller);
                let mem_base_ptr = data.as_ptr() as *mut u8;
                let mut total = 0usize;
                let current_slot = unsafe { TASK_MANAGER.get_current_slot() as usize };
                println!("sys_recvfrom {}", fd);
                for i in 0..ri_data_len as usize {
                    let base = ri_data_ptr as usize + i * 8;
                    if base + 8 > data.len() {
                        break;
                    }
                    let buf_ptr =
                        u32::from_le_bytes(data[base..base + 4].try_into().unwrap()) as usize;
                    let buf_len =
                        u32::from_le_bytes(data[base + 4..base + 8].try_into().unwrap()) as usize;
                    if buf_ptr
                        .checked_add(buf_len)
                        .map_or(true, |end| end > data.len())
                    {
                        break;
                    }

                    let buf_slice = unsafe {
                        core::slice::from_raw_parts_mut(mem_base_ptr.add(buf_ptr), buf_len)
                    };
                    let read = unsafe {
                        sys_recvfrom(
                            current_slot,
                            fd as usize,
                            buf_slice.as_mut_ptr(),
                            buf_slice.len(),
                        )
                    };
                    total += read;
                    if read < buf_len {
                        break;
                    }
                }

                memory
                    .write(
                        &mut caller,
                        ro_datalen_ptr as usize,
                        &(total as u32).to_le_bytes(),
                    )
                    .map_err(|_| wasmi::core::Trap::new("write datalen failed"))?;
                memory
                    .write(&mut caller, ro_flags_ptr as usize, &0u32.to_le_bytes())
                    .map_err(|_| wasmi::core::Trap::new("write flags failed"))?;
                Ok(0)
            })
        },
    )?;

    // sock_send
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "sock_send",
        |mut caller: wasmi::Caller<'_, ()>,
         fd: i32,
         si_data_ptr: i32,
         si_data_len: i32,
         _si_flags: i32,
         so_datalen_ptr: i32|
         -> Result<u32, wasmi::core::Trap> {
            without_interrupts(|| {
                let memory = caller
                    .get_export("memory")
                    .and_then(|ext| ext.into_memory())
                    .ok_or_else(|| wasmi::core::Trap::new("memory not found"))?;
                let data = memory.data(&caller);
                let mut total = 0usize;
                let current_slot = unsafe { TASK_MANAGER.get_current_slot() as usize };
                println!("sys_sendto {}", fd);

                for i in 0..si_data_len as usize {
                    let base = si_data_ptr as usize + i * 8;
                    if base + 8 > data.len() {
                        break;
                    }
                    let buf_ptr =
                        u32::from_le_bytes(data[base..base + 4].try_into().unwrap()) as usize;
                    let buf_len =
                        u32::from_le_bytes(data[base + 4..base + 8].try_into().unwrap()) as usize;
                    if buf_ptr
                        .checked_add(buf_len)
                        .map_or(true, |end| end > data.len())
                    {
                        break;
                    }

                    let buf_slice = &data[buf_ptr..buf_ptr + buf_len];
                    let written = unsafe {
                        sys_sendto(
                            current_slot,
                            fd as usize,
                            buf_slice.as_ptr(),
                            buf_slice.len(),
                        )
                    };
                    total += written;
                }

                memory
                    .write(
                        &mut caller,
                        so_datalen_ptr as usize,
                        &(total as u32).to_le_bytes(),
                    )
                    .map_err(|_| wasmi::core::Trap::new("write datalen failed"))?;
                Ok(0)
            })
        },
    )?;
    // 2. random_get (КРИТИЧНО для старта Rust std)
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "random_get",
        |mut caller: wasmi::Caller<'_, ()>,
         buf_ptr: i32,
         buf_len: i32|
         -> Result<u32, wasmi::core::Trap> {
            let memory = caller
                .get_export("memory")
                .and_then(|ext| ext.into_memory())
                .ok_or_else(|| wasmi::core::Trap::new("memory not found"))?;

            // ИСПОЛЬЗУЕМ data_mut для получения мутабельного среза памяти
            let data = memory.data_mut(&mut caller);
            let start = buf_ptr as usize;
            let len = buf_len as usize;

            if start.saturating_add(len) > data.len() {
                return Ok(21); // WASI_EINVAL
            }

            // Заполняем псевдослучайными данными (заглушка)
            let slice = &mut data[start..start + len];
            for (i, byte) in slice.iter_mut().enumerate() {
                *byte = (i as u8).wrapping_add(0x5A);
            }
            Ok(0) // WASI_ESUCCESS
        },
    )?;

    // 3. clock_time_get (Часто вызывается при инициализации)
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "clock_time_get",
        |mut caller: wasmi::Caller<'_, ()>,
         _clock_id: i32,
         _precision: i64,
         time_ptr: i32|
         -> Result<u32, wasmi::core::Trap> {
            let memory = caller
                .get_export("memory")
                .and_then(|ext| ext.into_memory())
                .ok_or_else(|| wasmi::core::Trap::new("memory not found"))?;

            // Возвращаем фейковое время в наносекундах (например, 1 секунда)
            let fake_time_ns: u64 = 1_000_000_000;
            memory
                .write(&mut caller, time_ptr as usize, &fake_time_ns.to_le_bytes())
                .map_err(|_| wasmi::core::Trap::new("write time failed"))?;
            Ok(0) // WASI_ESUCCESS
        },
    )?;

    Ok(())
}

/// Контекст для WASI
struct WasmTaskContext {
    slot: usize,
}

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
    if bytecode.is_empty() {
        return usize::MAX;
    }

    let params = read_exec_params(params_ptr);
    let slot_i8 = unsafe { TASK_MANAGER.get_free_slot() };
    if slot_i8 < 0 {
        return usize::MAX;
    }
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
            - core::mem::size_of::<CPUState>()) as *mut CPUState;
        task.cpu_state_ptr = state_ptr as u32;

        // 2. Настраиваем WASM
        let mut linker = Linker::new(&engine);
        let ctx = WasmTaskContext { slot };
        register_wasi_functions(&mut linker, &ctx).unwrap();
        let mut store = Store::new(&engine, ());
        let memory_type = MemoryType::new(1, Some(32)).unwrap();
        let memory = Memory::new(&mut store, memory_type).unwrap();
        linker
            .define("env", "memory", Extern::Memory(memory.clone()))
            .unwrap();
        let instance = linker
            .instantiate(&mut store, &module)
            .unwrap()
            .start(&mut store)
            .unwrap();

        // 3. Сохраняем состояние в глобальный массив
        WASM_EXEC[slot] = Some(Box::new(WasmExec { store, instance }));

        // 4. Настраиваем CPUState для KERNEL MODE (Ring 0)!
        *state_ptr = CPUState {
            eax: 0,
            ebx: 0,
            ecx: 0,
            edx: 0,
            esi: 0,
            edi: 0,
            ebp: 0,
            eip: wasm_task_entry as u32,
            cs: 0x08, // Kernel Code Segment (не 0x1B!)
            eflags: 0x202,
            esp: kernel_stack_top - 12,
            ss: 0x10, // Kernel Data Segment (не 0x23!)
        };

        task.running = true;
        task.parent = parent_slot as i8;
        task.zombie = false;
        task.exit_code = 0;

        let mut fd_table = FileDescriptorTable::with_stdio();
        install_child_fd(
            parent_slot,
            &mut fd_table,
            0,
            params.stdin,
            FileDescriptor::ConsoleIn,
        );
        install_child_fd(
            parent_slot,
            &mut fd_table,
            1,
            params.stdout,
            FileDescriptor::ConsoleOut,
        );
        install_child_fd(
            parent_slot,
            &mut fd_table,
            2,
            params.stderr,
            FileDescriptor::ConsoleOut,
        );
        task.fd_table = fd_table;

        TASK_MANAGER.tasks[slot] = Some(task);
        TASK_MANAGER.task_count += 1;
        sti!();
        slot
    }
}

/// Точка входа для Wasm-задачи
#[unsafe(no_mangle)]
extern "C" fn wasm_task_entry() -> ! {
    let slot = unsafe { TASK_MANAGER.get_current_slot() as usize };

    // Забираем WASM-состояние из глобального массива
    let exec = unsafe { WASM_EXEC[slot].take() };

    if let Some(mut exec) = exec {
        // Пытаемся найти и вызвать точку входа (_start или main)
        if let Some(start_func) = exec.instance.get_func(&mut exec.store, "_start") {
            let _ = match start_func.call(&mut exec.store, &[], &mut []) {
                Ok(_) => {}
                Err(err) => {
                    println!("wasm_task_entry _start call failed: {}", err);
                }
            };
        } else if let Some(main_func) = exec.instance.get_func(&mut exec.store, "main") {
            match main_func.call(&mut exec.store, &[], &mut []) {
                Ok(_) => {}
                Err(err) => {
                    println!("wasm_task_entry main call failed: {}", err);
                }
            }
        }
    }
    println!("Task {} exit", slot);
    // Завершаем задачу
    unsafe {
        sys_exit(slot, 0);
    }

    loop {
        unsafe { hlt!() }
    }
}
