use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::ops::Deref;

use crate::transactions::{RepLog, StorageTransaction};
use cosmwasm_std::{
    from_slice, Api, Binary, BlockInfo, ContractInfo, Deps, DepsMut, Env, HumanAddr, MessageInfo,
    Querier, QuerierWrapper, Response, Storage, WasmQuery,
};

/// Interface to call into a Contract
pub trait Contract {
    fn handle(
        &self,
        deps: DepsMut,
        env: Env,
        info: MessageInfo,
        msg: Vec<u8>,
    ) -> Result<Response, String>;

    fn init(
        &self,
        deps: DepsMut,
        env: Env,
        info: MessageInfo,
        msg: Vec<u8>,
    ) -> Result<Response, String>;

    fn query(&self, deps: Deps, env: Env, msg: Vec<u8>) -> Result<Binary, String>;
}

type ContractFn<T, R, E> = fn(deps: DepsMut, env: Env, info: MessageInfo, msg: T) -> Result<R, E>;

type QueryFn<T, E> = fn(deps: Deps, env: Env, msg: T) -> Result<Binary, E>;

/// Wraps the exported functions from a contract and provides the normalized format
/// TODO: Allow to customize return values (CustomMsg beyond Empty)
pub struct ContractWrapper<T1, T2, T3, E1, E2, E3>
where
    T1: DeserializeOwned,
    T2: DeserializeOwned,
    T3: DeserializeOwned,
    E1: std::fmt::Display,
    E2: std::fmt::Display,
    E3: std::fmt::Display,
{
    handle_fn: ContractFn<T1, Response, E1>,
    init_fn: ContractFn<T2, Response, E2>,
    query_fn: QueryFn<T3, E3>,
}

impl<T1, T2, T3, E1, E2, E3> ContractWrapper<T1, T2, T3, E1, E2, E3>
where
    T1: DeserializeOwned,
    T2: DeserializeOwned,
    T3: DeserializeOwned,
    E1: std::fmt::Display,
    E2: std::fmt::Display,
    E3: std::fmt::Display,
{
    pub fn new(
        handle_fn: ContractFn<T1, Response, E1>,
        init_fn: ContractFn<T2, Response, E2>,
        query_fn: QueryFn<T3, E3>,
    ) -> Self {
        ContractWrapper {
            handle_fn,
            init_fn,
            query_fn,
        }
    }
}

impl<T1, T2, T3, E1, E2, E3> Contract for ContractWrapper<T1, T2, T3, E1, E2, E3>
where
    T1: DeserializeOwned,
    T2: DeserializeOwned,
    T3: DeserializeOwned,
    E1: std::fmt::Display,
    E2: std::fmt::Display,
    E3: std::fmt::Display,
{
    fn handle(
        &self,
        deps: DepsMut,
        env: Env,
        info: MessageInfo,
        msg: Vec<u8>,
    ) -> Result<Response, String> {
        let msg: T1 = from_slice(&msg).map_err(|e| e.to_string())?;
        let res = (self.handle_fn)(deps, env, info, msg);
        res.map_err(|e| e.to_string())
    }

    fn init(
        &self,
        deps: DepsMut,
        env: Env,
        info: MessageInfo,
        msg: Vec<u8>,
    ) -> Result<Response, String> {
        let msg: T2 = from_slice(&msg).map_err(|e| e.to_string())?;
        let res = (self.init_fn)(deps, env, info, msg);
        res.map_err(|e| e.to_string())
    }

    fn query(&self, deps: Deps, env: Env, msg: Vec<u8>) -> Result<Binary, String> {
        let msg: T3 = from_slice(&msg).map_err(|e| e.to_string())?;
        let res = (self.query_fn)(deps, env, msg);
        res.map_err(|e| e.to_string())
    }
}

struct ContractData {
    code_id: usize,
    storage: Box<dyn Storage>,
}

impl ContractData {
    fn new(code_id: usize, storage: Box<dyn Storage>) -> Self {
        ContractData { code_id, storage }
    }
}

pub fn next_block(block: &mut BlockInfo) {
    block.time += 5;
    block.height += 1;
}

pub type StorageFactory = fn() -> Box<dyn Storage>;

pub struct WasmRouter {
    // WasmState - cache this, pass in separate?
    handlers: HashMap<usize, Box<dyn Contract>>,
    contracts: HashMap<HumanAddr, ContractData>,
    // WasmConst
    block: BlockInfo,
    api: Box<dyn Api>,
    storage_factory: StorageFactory,
}

impl WasmRouter {
    pub fn new(api: Box<dyn Api>, block: BlockInfo, storage_factory: StorageFactory) -> Self {
        WasmRouter {
            handlers: HashMap::new(),
            contracts: HashMap::new(),
            block,
            api,
            storage_factory,
        }
    }

    pub fn set_block(&mut self, block: BlockInfo) {
        self.block = block;
    }

    // this let's use use "next block" steps that add eg. one height and 5 seconds
    pub fn update_block<F: Fn(&mut BlockInfo)>(&mut self, action: F) {
        action(&mut self.block);
    }

    /// Returns a copy of the current block_info
    pub fn block_info(&self) -> BlockInfo {
        self.block.clone()
    }

    pub fn store_code(&mut self, code: Box<dyn Contract>) -> usize {
        let idx = self.handlers.len() + 1;
        self.handlers.insert(idx, code);
        idx
    }

    pub fn cache(&'_ self) -> WasmCache<'_> {
        WasmCache::new(self)
    }

    pub fn query(&self, querier: &dyn Querier, request: WasmQuery) -> Result<Binary, String> {
        match request {
            WasmQuery::Smart { contract_addr, msg } => {
                self.query_smart(contract_addr, querier, msg.into())
            }
            WasmQuery::Raw { contract_addr, key } => self.query_raw(contract_addr, &key),
            q => panic!("Unsupported wasm query: {:?}", q),
        }
    }

    pub fn query_smart(
        &self,
        address: HumanAddr,
        querier: &dyn Querier,
        msg: Vec<u8>,
    ) -> Result<Binary, String> {
        self.with_storage(querier, address, |handler, deps, env| {
            handler.query(deps, env, msg)
        })
    }

    pub fn query_raw(&self, address: HumanAddr, key: &[u8]) -> Result<Binary, String> {
        let contract = self
            .contracts
            .get(&address)
            .ok_or_else(|| "Unregistered contract address".to_string())?;
        let data = contract.storage.get(&key).unwrap_or_default();
        Ok(data.into())
    }

    fn get_env<T: Into<HumanAddr>>(&self, address: T) -> Env {
        Env {
            block: self.block.clone(),
            contract: ContractInfo {
                address: address.into(),
            },
        }
    }

    fn with_storage<F, T>(
        &self,
        querier: &dyn Querier,
        address: HumanAddr,
        action: F,
    ) -> Result<T, String>
    where
        F: FnOnce(&Box<dyn Contract>, Deps, Env) -> Result<T, String>,
    {
        let contract = self
            .contracts
            .get(&address)
            .ok_or_else(|| "Unregistered contract address".to_string())?;
        let handler = self
            .handlers
            .get(&contract.code_id)
            .ok_or_else(|| "Unregistered code id".to_string())?;
        let env = self.get_env(address);

        let deps = Deps {
            storage: contract.storage.as_ref(),
            api: self.api.deref(),
            querier: QuerierWrapper::new(querier),
        };
        action(handler, deps, env)
    }
}

/// A writable transactional cache over the wasm state.
///
/// Reads hit local hashmap or then hit router
/// Getting storage wraps the internal contract storage
///  - adding handler
///  - adding contract
///  - writing existing contract
/// Return op-log to flush, like transactional:
///  - consume this struct (release router) and return list of ops to perform
///  - pass ops &mut WasmRouter to update them
///
/// In Router, we use this exclusively in all the calls in execute (not self.wasm)
/// In Querier, we use self.wasm
pub struct WasmCache<'a> {
    // and this into one with reference
    router: &'a WasmRouter,
    state: WasmCacheState<'a>,
}

/// This is the mutable state of the cached.
/// Separated out so we can grab a mutable reference to both these HashMaps,
/// while still getting an immutable reference to router.
/// (We cannot take &mut WasmCache)
pub struct WasmCacheState<'a> {
    contracts: HashMap<HumanAddr, ContractData>,
    contract_diffs: HashMap<HumanAddr, StorageTransaction<'a>>,
}

/// This is a set of data from the WasmCache with no external reference,
/// which can be used to commit to the underlying WasmRouter.
pub struct WasmOps {
    new_contracts: HashMap<HumanAddr, ContractData>,
    contract_diffs: Vec<(HumanAddr, RepLog)>,
}

impl WasmOps {
    pub fn commit(self, router: &mut WasmRouter) {
        self.new_contracts.into_iter().for_each(|(k, v)| {
            router.contracts.insert(k, v);
        });
        self.contract_diffs.into_iter().for_each(|(k, ops)| {
            let storage = router.contracts.get_mut(&k).unwrap().storage.as_mut();
            ops.commit(storage);
        });
    }
}

impl<'a> WasmCache<'a> {
    fn new(router: &'a WasmRouter) -> Self {
        WasmCache {
            router,
            state: WasmCacheState {
                contracts: HashMap::new(),
                contract_diffs: HashMap::new(),
            },
        }
    }

    /// When we want to commit the WasmCache, we need a 2 step process to satisfy Rust reference counting:
    /// 1. prepare() consumes WasmCache, releasing &WasmRouter, and creating a self-owned update info.
    /// 2. WasmOps::commit() can now take &mut WasmRouter and updates the underlying state
    pub fn prepare(self) -> WasmOps {
        self.state.prepare()
    }

    /// This just creates an address and empty storage instance, returning the new address
    /// You must call init after this to set up the contract properly.
    /// These are separated into two steps to have cleaner return values.
    pub fn register_contract(&mut self, code_id: usize) -> Result<HumanAddr, String> {
        if !self.router.handlers.contains_key(&code_id) {
            return Err("Cannot init contract with unregistered code id".to_string());
        }
        let addr = self.next_address();
        let info = ContractData::new(code_id, (self.router.storage_factory)());
        self.state.contracts.insert(addr.clone(), info);
        Ok(addr)
    }

    // TODO: better addr generation
    fn next_address(&self) -> HumanAddr {
        let count = self.router.contracts.len() + self.state.contracts.len();
        // we make this longer so it is not rejected by tests
        HumanAddr::from("Contract #".to_string() + &count.to_string())
    }

    pub fn handle(
        &mut self,
        address: HumanAddr,
        querier: &dyn Querier,
        info: MessageInfo,
        msg: Vec<u8>,
    ) -> Result<Response, String> {
        let parent = &self.router.handlers;
        let contracts = &self.router.contracts;
        let env = self.router.get_env(address.clone());
        let api = self.router.api.as_ref();

        self.state.with_storage(
            querier,
            contracts,
            address,
            env,
            api,
            |code_id, deps, env| {
                let handler = parent
                    .get(&code_id)
                    .ok_or_else(|| "Unregistered code id".to_string())?;
                handler.handle(deps, env, info, msg)
            },
        )
    }

    pub fn init(
        &mut self,
        address: HumanAddr,
        querier: &dyn Querier,
        info: MessageInfo,
        msg: Vec<u8>,
    ) -> Result<Response, String> {
        let parent = &self.router.handlers;
        let contracts = &self.router.contracts;
        let env = self.router.get_env(address.clone());
        let api = self.router.api.as_ref();

        self.state.with_storage(
            querier,
            contracts,
            address,
            env,
            api,
            |code_id, deps, env| {
                let handler = parent
                    .get(&code_id)
                    .ok_or_else(|| "Unregistered code id".to_string())?;
                handler.init(deps, env, info, msg)
            },
        )
    }
}

impl<'a> WasmCacheState<'a> {
    pub fn prepare(self) -> WasmOps {
        let diffs: Vec<_> = self
            .contract_diffs
            .into_iter()
            .map(|(k, store)| (k, store.prepare()))
            .collect();

        WasmOps {
            new_contracts: self.contracts,
            contract_diffs: diffs,
        }
    }

    fn get_contract<'b>(
        &'b mut self,
        parent: &'a HashMap<HumanAddr, ContractData>,
        addr: &HumanAddr,
    ) -> Option<(usize, &'b mut dyn Storage)> {
        // if we created this transaction
        if let Some(x) = self.contracts.get_mut(addr) {
            return Some((x.code_id, x.storage.as_mut()));
        }
        if let Some(c) = parent.get(addr) {
            let code_id = c.code_id;
            if self.contract_diffs.contains_key(addr) {
                let storage = self.contract_diffs.get_mut(addr).unwrap();
                return Some((code_id, storage));
            }
            // else make a new transaction
            let wrap = StorageTransaction::new(c.storage.as_ref());
            self.contract_diffs.insert(addr.clone(), wrap);
            Some((code_id, self.contract_diffs.get_mut(addr).unwrap()))
        } else {
            None
        }
    }

    fn with_storage<F, T>(
        &mut self,
        querier: &dyn Querier,
        parent: &'a HashMap<HumanAddr, ContractData>,
        address: HumanAddr,
        env: Env,
        api: &dyn Api,
        action: F,
    ) -> Result<T, String>
    where
        F: FnOnce(usize, DepsMut, Env) -> Result<T, String>,
    {
        let (code_id, storage) = self
            .get_contract(parent, &address)
            .ok_or_else(|| "Unregistered contract address".to_string())?;
        let deps = DepsMut {
            storage,
            api,
            querier: QuerierWrapper::new(querier),
        };
        action(code_id, deps, env)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use crate::test_helpers::{contract_error, contract_payout, PayoutMessage};
    use cosmwasm_std::testing::{mock_env, mock_info, MockApi, MockQuerier, MockStorage};
    use cosmwasm_std::{coin, to_vec, BankMsg, BlockInfo, CosmosMsg, Empty};

    fn mock_router() -> WasmRouter {
        let env = mock_env();
        let api = Box::new(MockApi::default());
        WasmRouter::new(api, env.block, || Box::new(MockStorage::new()))
    }

    #[test]
    fn register_contract() {
        let mut router = mock_router();
        let code_id = router.store_code(contract_error());
        let mut cache = router.cache();

        // cannot register contract with unregistered codeId
        cache.register_contract(code_id + 1).unwrap_err();

        // we can register a new instance of this code
        let contract_addr = cache.register_contract(code_id).unwrap();

        // now, we call this contract and see the error message from the contract
        let querier: MockQuerier<Empty> = MockQuerier::new(&[]);
        let info = mock_info("foobar", &[]);
        let err = cache
            .init(contract_addr, &querier, info, b"{}".to_vec())
            .unwrap_err();
        // StdError from contract_error auto-converted to string
        assert_eq!(err, "Generic error: Init failed");

        // and the error for calling an unregistered contract
        let info = mock_info("foobar", &[]);
        let err = cache
            .init("unregistered".into(), &querier, info, b"{}".to_vec())
            .unwrap_err();
        // Default error message from router when not found
        assert_eq!(err, "Unregistered contract address");

        // and flush
        cache.prepare().commit(&mut router);
    }

    #[test]
    fn update_block() {
        let mut router = mock_router();

        let BlockInfo { time, height, .. } = router.get_env("foo").block;
        router.update_block(next_block);
        let next = router.get_env("foo").block;

        assert_eq!(time + 5, next.time);
        assert_eq!(height + 1, next.height);
    }

    #[test]
    fn contract_send_coins() {
        let mut router = mock_router();
        let code_id = router.store_code(contract_payout());
        let mut cache = router.cache();

        let contract_addr = cache.register_contract(code_id).unwrap();

        let querier: MockQuerier<Empty> = MockQuerier::new(&[]);
        let payout = coin(100, "TGD");

        // init the contract
        let info = mock_info("foobar", &[]);
        let init_msg = to_vec(&PayoutMessage {
            payout: payout.clone(),
        })
        .unwrap();
        let res = cache
            .init(contract_addr.clone(), &querier, info, init_msg)
            .unwrap();
        assert_eq!(0, res.messages.len());

        // execute the contract
        let info = mock_info("foobar", &[]);
        let res = cache
            .handle(contract_addr.clone(), &querier, info, b"{}".to_vec())
            .unwrap();
        assert_eq!(1, res.messages.len());
        match &res.messages[0] {
            CosmosMsg::Bank(BankMsg::Send { to_address, amount }) => {
                assert_eq!(to_address.as_str(), "foobar");
                assert_eq!(amount.as_slice(), &[payout.clone()]);
            }
            m => panic!("Unexpected message {:?}", m),
        }

        // and flush before query
        cache.prepare().commit(&mut router);

        // query the contract
        let data = router
            .query_smart(contract_addr.clone(), &querier, b"{}".to_vec())
            .unwrap();
        let res: PayoutMessage = from_slice(&data).unwrap();
        assert_eq!(res.payout, payout);
    }
}
