use std::error::Error;
use std::fs::File;
use std::io::prelude::*;
use std::path::Path;

//extern crate clap;
//use clap::{App, Arg, SubCommand};

extern crate dimsum;
//extern crate env_logger;
#[macro_use]
extern crate log;
//extern crate rand;
//extern crate serde;
//extern crate serde_json;

//extern crate futures;
//extern crate hyper;
#[macro_use]
//extern crate serde_derive;
//extern crate tokio_core;

mod ast;
mod error;
//mod fetcher;
mod interpreter;
mod lexer;
mod parser;
mod tokenid;

//use fetcher::Fetcher;
use interpreter::Interpreter;
use lexer::Lexer;
use parser::Parser;

pub fn run_prog(prog: String) -> String {
    //env_logger::init();

    //let mut prog = String::new();
    //prog.push_str("10 PRINT \"HELLO\"\n");
    let lexer = Lexer::new(prog);
    let ast = match Parser::new(lexer).parse() {
        Ok(ast) => ast,
        Err(e) => {
            println!("{}", e);
            return String::from(format!("Error {}", e).as_str())
            //std::process::exit(1);
        }
    };

    let mut interpreter = Interpreter::new();
    return match interpreter.run(&ast) {
        Ok(_) => interpreter.stdout.iter().map(|x| x.to_string()).collect::<Vec<_>>().join("\n"),
        Err(e) => {
            println!("{}", e);
            String::from(format!("Error {}", e).as_str())
            //std::process::exit(1)
        }
    };
}
