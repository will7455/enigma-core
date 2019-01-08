#![allow(dead_code,unused_assignments,unused_variables)]
extern crate sgx_types;
extern crate sgx_urts;
extern crate rustc_hex;

use common_u::errors::EnclaveFailError;
use enigma_types::EnclaveReturn;
use enigma_types::traits::SliceCPtr;
use failure::Error;
use sgx_types::*;

extern "C" {
    fn ecall_deploy(eid: sgx_enclave_id_t, retval: *mut EnclaveReturn, bytecode: *const u8, bytecode_len: usize,
                    gas_limit: *const u64, output_ptr: *mut u64) -> sgx_status_t;

    fn ecall_execute(eid: sgx_enclave_id_t, retval: *mut EnclaveReturn,
                     bytecode: *const u8, bytecode_len: usize,
                     callable: *const u8, callable_len: usize,
                     callable_args: *const u8, callable_args_len: usize,
                     gas_limit: *const u64,
                     output: *mut u64, delta_data_ptr: *mut u64,
                     delta_hash_out: &mut [u8; 32], delta_index_out: *mut u32,
                     ethereum_payload: *mut u64,
                     ethereum_contract_addr: &mut [u8; 20]) -> sgx_status_t;
}

const MAX_EVM_RESULT: usize = 100_000;
pub fn deploy(eid: sgx_enclave_id_t,  bytecode: &[u8], gas_limit: u64)-> Result<Box<[u8]>, Error> {
    let mut retval = EnclaveReturn::Success;
    let mut output_ptr: u64 = 0;

    let status = unsafe {
        ecall_deploy(eid,
                     &mut retval,
                     bytecode.as_c_ptr() as *const u8,
                     bytecode.len(),
                     &gas_limit as *const u64,
                     &mut output_ptr as *mut u64)
    };
    if retval != EnclaveReturn::Success  || status != sgx_status_t::SGX_SUCCESS {
        return Err(EnclaveFailError{err: retval, status}.into());
    }
    let box_ptr = output_ptr as *mut Box<[u8]>;
    let part = unsafe { Box::from_raw(box_ptr ) };
    Ok(*part)
}

#[derive(Clone, Debug, PartialEq, PartialOrd, Eq, Ord, Hash, Default)]
pub struct WasmResult {
    pub bytecode: Vec<u8>,
    pub output: Vec<u8>,
    pub delta: ::db::Delta,
    pub eth_payload: Vec<u8>,
    pub eth_contract_addr: [u8;20],
}

pub fn execute(eid: sgx_enclave_id_t,  bytecode: &[u8], callable: &str, args: &[u8], gas_limit: u64)-> Result<WasmResult,Error>{
    let mut retval: EnclaveReturn = EnclaveReturn::Success;
    let mut output = 0u64;
    let mut delta_data_ptr = 0u64;
    let mut delta_hash = [0u8; 32];
    let mut delta_index = 0u32;
    let mut ethereum_payload = 0u64;
    let mut ethereum_contract_addr = [0u8; 20];

    let status = unsafe {
        ecall_execute(eid,
                      &mut retval,
                      bytecode.as_c_ptr() as *const u8,
                      bytecode.len(),
                      callable.as_c_ptr() as *const u8,
                      callable.len(),
                      args.as_c_ptr() as *const u8,
                      args.len(),
                      &gas_limit as *const u64,
                      &mut output as *mut u64,
                      &mut delta_data_ptr as *mut u64,
                      &mut delta_hash,
                      &mut delta_index as *mut u32,
                      &mut ethereum_payload as *mut u64,
                      &mut ethereum_contract_addr)
    };

    if retval != EnclaveReturn::Success  || status != sgx_status_t::SGX_SUCCESS {
        return Err(EnclaveFailError{err: retval, status}.into());
    }
    // TODO: Write a handle wrapper that will free the pointers memory in case of an Error.

    let mut result: WasmResult = Default::default();
    let box_ptr = output as *mut Box<[u8]>;
    let output = unsafe { Box::from_raw(box_ptr) };
    result.output = output.to_vec();
    let box_payload_ptr = ethereum_payload as *mut Box<[u8]>;
    let payload = unsafe { Box::from_raw(box_payload_ptr) };
    result.eth_payload = payload.to_vec();
    result.eth_contract_addr = ethereum_contract_addr;
    if delta_data_ptr != 0 && delta_hash != [0u8; 32] && delta_index != 0 {
        // TODO: Replace 0 with maybe max int(accordingly).
        let box_ptr = delta_data_ptr as *mut Box<[u8]>;
        let delta_data = unsafe { Box::from_raw(box_ptr) };
        result.delta.value = delta_data.to_vec();
        // TODO: Elichai look at this please.
        use db::{DeltaKey, Stype};
        result.delta.key = DeltaKey::new(delta_hash, Stype::Delta(delta_index));
    } else {
        bail!("Weird delta results")
    }
    Ok(result)
}

#[cfg(test)]
pub mod tests {
    #![allow(dead_code, unused_assignments, unused_variables)]

    use esgx;
    use sgx_urts::SgxEnclave;
    use std::fs::File;
    use std::io::Read;
    use std::path::PathBuf;
    use sgx_types::*;
    use std::process::Command;
    use wasm_u::wasm;
    use std::str::from_utf8;

    fn init_enclave() -> SgxEnclave {
        let enclave = match esgx::general::init_enclave_wrapper() {
            Ok(r) => {
                println!("[+] Init Enclave Successful {}!", r.geteid());
                r
            }
            Err(x) => {
                panic!("[-] Init Enclave Failed {}!", x.as_str());
            }
        };
        enclave
    }

    fn compile_and_deploy_wasm_contract(eid: sgx_enclave_id_t, test_path: &str) -> Box<[u8]>{
        let mut dir = PathBuf::new();
        dir.push(test_path);
        let mut output = Command::new("cargo")
            .current_dir(&dir)
            .args(&["build", "--release"])
            .spawn()
            .expect(&format!("Failed compiling wasm contract: {:?}", &dir) );

        assert!(output.wait().unwrap().success());
        dir.push("target/wasm32-unknown-unknown/release/contract.wasm");

        let mut f = File::open(&dir).expect(&format!("Can't open the contract.wasm file: {:?}", &dir));
        let mut wasm_code = Vec::new();
        f.read_to_end(&mut wasm_code).expect("Failed reading the wasm file");
        println!("Bytecode size: {}KB\n", wasm_code.len()/1024);
        wasm::deploy(eid, &wasm_code, 100_000).expect("Deploy Failed")
    }

    #[test]
    fn test_print_simple() {
        let enclave = init_enclave();
        let contract_code = compile_and_deploy_wasm_contract(enclave.geteid(), "../../examples/eng_wasm_contracts/simplest");
        let encrypted_args : &[u8] = &[38, 69, 69, 163, 224, 68, 24, 62, 147, 1, 82, 167, 140, 186, 10, 139, 101, 150, 13, 23, 125, 201, 203, 83, 111, 166, 187, 6, 193, 251, 218, 180, 102, 106, 46, 227, 128, 132, 183, 241, 25, 162, 23, 4, 192, 231, 147, 7, 242, 85, 161, 62, 241, 193, 32, 170, 123, 51, 34, 222, 36, 58, 140, 1, 96, 218, 76, 52, 223, 220, 132, 149, 109, 223, 84, 126, 46, 222, 119, 245, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
        let result = wasm::execute(enclave.geteid(),&contract_code, "print_test(uint256,uint256)", encrypted_args, 100_000).expect("Execution failed");
        assert_eq!(from_utf8(&result.output).unwrap(), "22");
        enclave.destroy();
    }

    #[test]
    fn test_write_simple() {
        let enclave = init_enclave();
        let contract_code = compile_and_deploy_wasm_contract(enclave.geteid(), "../../examples/eng_wasm_contracts/simplest");
        let args : &[u8] = &[];
        let result = wasm::execute(enclave.geteid(), &contract_code, "write()", args, 100_000).expect("Execution failed");
        enclave.destroy();
        assert_eq!(from_utf8(&result.output).unwrap(), "\"157\"");
    }

    #[test]
    fn test_address_simple() {
        let enclave = init_enclave();
        let contract_code = compile_and_deploy_wasm_contract(enclave.geteid(), "../../examples/eng_wasm_contracts/simplest");
        // args must be solidity abi serialized and encrypted: 0x5ed8cee6b63b1c6afce3ad7c92f4fd7e1b8fad9f
        let arg : &[u8] = &[38, 69, 69, 163, 224, 68, 24, 62, 147, 1, 82, 167, 210, 98, 196, 109, 211, 173, 17, 125, 129, 42, 102, 47, 253, 82, 70, 120, 218, 116, 119, 36, 240, 59, 66, 156, 217, 122, 131, 200, 114, 72, 241, 86, 8, 101, 232, 52, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
        let result = wasm::execute(enclave.geteid(), &contract_code, "get_address(address)", arg, 100_000).expect("Execution failed");
        enclave.destroy();
        assert_eq!(from_utf8(&result.output).unwrap(), "\"5ed8cee6b63b1c6afce3ad7c92f4fd7e1b8fad9f\"");
    }

    #[test]
    fn test_addresses_simple() {
        let enclave = init_enclave();
        let contract_code = compile_and_deploy_wasm_contract(enclave.geteid(), "../../examples/eng_wasm_contracts/simplest");
        // args must be solidity abi serialized and encrypted: 0x5ed8cee6b63b1c6afce3ad7c92f4fd7e1b8fad9f, 0xde0b295669a9fd93d5f28d9ec85e40f4cb697bae
        let arg : &[u8] = &[38, 69, 69, 163, 224, 68, 24, 62, 147, 1, 82, 167, 210, 98, 196, 109, 211, 173, 17, 125, 129, 42, 102, 47, 253, 82, 70, 120, 218, 116, 119, 36, 102, 106, 46, 227, 128, 132, 183, 241, 25, 162, 23, 4, 30, 236, 186, 81, 155, 252, 92, 173, 36, 51, 173, 52, 179, 109, 98, 42, 239, 83, 247, 185, 157, 218, 32, 129, 14, 171, 201, 97, 183, 214, 96, 219, 155, 125, 63, 2, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
        let result = wasm::execute(enclave.geteid(), &contract_code, "get_addresses(address,address)", arg, 100_000).expect("Execution failed");
        enclave.destroy();
        assert_eq!(from_utf8(&result.output).unwrap(), "\"de0b295669a9fd93d5f28d9ec85e40f4cb697bae\"");
    }

    #[test]
    fn test_add_erc20() {
        let enclave = init_enclave();
        let contract_code = compile_and_deploy_wasm_contract(enclave.geteid(), "../../examples/eng_wasm_contracts/erc20");
        // args must be solidity abi serialized and encrypted: 0x5ed8cee6b63b1c6afce3ad7c92f4fd7e1b8fad9f,0x07
        let arg : &[u8] = &[38, 69, 69, 163, 224, 68, 24, 62, 147, 1, 82, 167, 210, 98, 196, 109, 211, 173, 17, 125, 129, 42, 102, 47, 253, 82, 70, 120, 218, 116, 119, 36, 102, 106, 46, 227, 128, 132, 183, 241, 25, 162, 23, 4, 192, 231, 147, 7, 242, 85, 161, 62, 241, 193, 32, 170, 123, 51, 34, 222, 36, 58, 140, 16, 132, 229, 33, 83, 242, 58, 112, 32, 106, 28, 159, 142, 169, 210, 251, 187, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
        let result = wasm::execute(enclave.geteid(), &contract_code, "add_to_balance(address,uint256)", arg, 100_000).expect("Execution failed");
        enclave.destroy();
        assert_eq!(from_utf8(&result.output).unwrap(), "7");
    }

    // todo: need to add an initial state in order to test the functionality of transfer
//    #[test]
//    fn test_transfer_erc20() {
//        let enclave = init_enclave();
//        let contract_code = compile_and_deploy_wasm_contract(enclave.geteid(), "../../examples/eng_wasm_contracts/erc20");
////         args before encryption and solidity abi serialization: ["5ed8cee6b63b1c6afce3ad7c92f4fd7e1b8fad9f","de0b295669a9fd93d5f28d9ec85e40f4cb697bae",0x03]
//        let rlp_args_transfer = &[38, 69, 69, 163, 224, 68, 24, 62, 147, 1, 82, 167, 210, 98, 196, 109, 211, 173, 17, 125, 129, 42, 102, 47, 253, 82, 70, 120, 218, 116, 119, 36, 102, 106, 46, 227, 128, 132, 183, 241, 25, 162, 23, 4, 30, 236, 186, 81, 155, 252, 92, 173, 36, 51, 173, 52, 179, 109, 98, 42, 239, 83, 247, 185, 177, 170, 21, 230, 102, 118, 94, 128, 129, 231, 172, 10, 30, 250, 148, 100, 106, 120, 76, 210, 247, 41, 176, 128, 89, 69, 95, 13, 135, 132, 164, 48, 197, 153, 96, 169, 17, 120, 160, 226, 23, 195, 27, 165, 169, 4, 69, 141, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
//        let res_addr_account = wasm::execute(enclave.geteid(), &contract_code, "transfer(string,string,uint256)", rlp_args_transfer, 100_000_000).expect("Execution failed");
//        enclave.destroy();
//
//        assert_eq!(from_utf8(&res_addr_account.output).unwrap(), "3");
//    }

    #[test]
    fn test_eth_bridge() {
        let enclave = init_enclave();
        let contract_code = compile_and_deploy_wasm_contract(enclave.geteid(), "../../examples/eng_wasm_contracts/contract_with_eth_calls");
        let arg: &[u8] = &[];
        let result = wasm::execute(enclave.geteid(), &contract_code, "test()", arg, 100_000).expect("Execution failed");
        enclave.destroy();
    }

    #[ignore]
    #[test]
    pub fn test_contract() {
        let mut f = File::open(
            "../../examples/eng_wasm_contracts/simplest/target/wasm32-unknown-unknown/release/contract.wasm",
        )
        .unwrap();
        let mut wasm_code = Vec::new();
        f.read_to_end(&mut wasm_code).unwrap();
        println!("Bytecode size: {}KB\n", wasm_code.len() / 1024);
        let enclave = init_enclave();
        let contract_code = wasm::deploy(enclave.geteid(), &wasm_code, 100_000).expect("Deploy Failed");
        let result = wasm::execute(enclave.geteid(),&contract_code, "call", &[], 100_000).expect("Execution failed");
        assert_eq!(from_utf8(&result.output).unwrap(), "157");
    }
}
