pub mod contract;
mod error;
mod integration_tests;
pub mod msg;
pub mod state;

#[cfg(all(target_arch = "wasm32", not(feature = "library")))]
cosmwasm_std::create_entry_points!(contract);
