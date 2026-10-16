// Gallery: `derive(ProviderSpec)` without `#[provides(..)]`.
// Fix: name the service, `#[provides(Counter, with = || Counter(0))]`.
use id_effect::ProviderSpecDerive;

pub struct Counter(pub u32);

#[derive(ProviderSpecDerive)]
pub struct CounterLive;

fn main() {}
