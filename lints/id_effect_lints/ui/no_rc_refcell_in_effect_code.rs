// G-04: `Rc<RefCell<…>>`.
#![allow(dead_code)]
use std::cell::RefCell;
use std::rc::Rc;

struct Cache {
  entries: Rc<RefCell<Vec<u8>>>,
  frozen: Rc<Vec<u8>>,
}

fn main() {}
