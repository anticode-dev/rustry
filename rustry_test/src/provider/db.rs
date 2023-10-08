use revm::{
    db::{CacheDB, EmptyDB},
    primitives::{
        alloy_primitives::Uint, Address, Bytes, Env, ExecutionResult, Halt, Output, TransactTo,
        U256,
    },
    EVM,
};

pub struct Provider {
    evm: EVM<CacheDB<EmptyDB>>,
    /// The active sender
    pub sender: Address,
}

impl Default for Provider {
    fn default() -> Self {
        let mut evm = EVM::new();
        evm.database(CacheDB::new(EmptyDB::default()));
        Provider {
            evm,
            sender: Address::ZERO,
        }
    }
}

pub enum ExecRes {
    Success(Bytes),
    Revert(Bytes),
    Halt(Halt),
}

impl Provider {
    fn env(&mut self) -> &mut Env {
        &mut self.evm.env
    }

    fn deploy_code(&mut self, from: Address, code: Bytes, value: Uint<256, 4>) -> Option<Address> {
        let tx = &mut self.env().tx;
        tx.caller = from;
        tx.transact_to = TransactTo::create();
        tx.data = code;
        tx.value = value;
        match self.evm.transact_commit().unwrap() {
            ExecutionResult::Success { output, .. } => match output {
                Output::Create(_, addr) => addr,
                _ => unreachable!(),
            },
            _ => None,
        }
    }

    fn call(&mut self, from: Address, to: Address, data: Bytes, value: Uint<256, 4>) -> ExecRes {
        let tx = &mut self.env().tx;
        tx.caller = from;
        tx.transact_to = TransactTo::Call(to);
        tx.data = data;
        tx.value = value;

        match self.evm.transact().unwrap().result {
            ExecutionResult::Success { output, .. } => ExecRes::Success(output.into_data()),
            ExecutionResult::Revert { output, .. } => ExecRes::Revert(output),
            ExecutionResult::Halt { reason, .. } => ExecRes::Halt(reason),
        }
    }

    fn send(&mut self, from: Address, to: Address, data: Bytes, value: Uint<256, 4>) -> ExecRes {
        let tx = &mut self.env().tx;
        tx.caller = from;
        tx.transact_to = TransactTo::Call(to);
        tx.data = data;
        tx.value = value;

        match self.evm.transact_commit().unwrap() {
            ExecutionResult::Success { output, .. } => ExecRes::Success(output.into_data()),
            ExecutionResult::Revert { output, .. } => ExecRes::Revert(output),
            ExecutionResult::Halt { reason, .. } => ExecRes::Halt(reason),
        }
    }
}

pub trait Frontend {
    fn deploy(&mut self, code: Bytes) -> Option<Address>;
    fn deploy_with_value(&mut self, code: Bytes, value: Uint<256, 4>) -> Option<Address>;
    fn call(&mut self, to: Address, data: Bytes) -> ExecRes;
    fn send(&mut self, to: Address, data: Bytes) -> ExecRes;
    fn send_value(&mut self, to: Address, data: Bytes, value: Uint<256, 4>) -> ExecRes;
}

impl Frontend for Provider {
    fn deploy(&mut self, code: Bytes) -> Option<Address> {
        self.deploy_code(self.sender, code, U256::ZERO)
    }

    fn deploy_with_value(&mut self, code: Bytes, value: Uint<256, 4>) -> Option<Address> {
        self.deploy_code(self.sender, code, value)
    }

    fn call(&mut self, to: Address, data: Bytes) -> ExecRes {
        self.call(self.sender, to, data, U256::ZERO)
    }

    fn send(&mut self, to: Address, data: Bytes) -> ExecRes {
        self.send(self.sender, to, data, U256::ZERO)
    }

    fn send_value(&mut self, to: Address, data: Bytes, value: Uint<256, 4>) -> ExecRes {
        self.send(self.sender, to, data, value)
    }
}

pub trait Cheats {
    fn impersonate(&mut self, who: Address);
}

impl Cheats for Provider {
    fn impersonate(&mut self, who: Address) {
        self.sender = who
    }
}