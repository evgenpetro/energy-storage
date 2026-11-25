//! Графічний інтерфейс для моніторингу акумуляторів на JBD BMS (Bluetooth LE).
//!
//! Ліва панель — список знайдених/підключених акумуляторів,
//! права панель — параметри обраного акумулятора.

mod jbd_bms;
mod gui;

fn main() -> iced::Result {
    // На цій машині немає Vulkan-драйвера (є лише loader, без ICD), тому
    // backend Vulkan у wgpu падає з STATUS_ACCESS_VIOLATION. Примусово DX12.
    #[cfg(windows)]
    // SAFETY: викликається на самому початку main, до створення будь-яких потоків.
    unsafe {
        if std::env::var_os("WGPU_BACKEND").is_none() {
            std::env::set_var("WGPU_BACKEND", "dx12");
        }
    }
    
    gui::init_app()
    
}

