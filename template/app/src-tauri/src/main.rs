// Без окна консоли в выпускной сборке Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    __APPLIB__::run()
}
