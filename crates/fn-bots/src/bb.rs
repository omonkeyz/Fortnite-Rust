use glam::Vec3;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum KeyType { Bool, Int, Enum, Float, Vector, Rotator, Object, Other }

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Val { Bool(bool), Int(i32), Float(f32), Vec(Option<Vec3>), Obj(Option<u64>) }

pub struct Schema { pub keys: Vec<(String, KeyType, Option<String>)>, pub index: HashMap<String, u16> }

impl Schema {
    pub fn from_json(v: &serde_json::Value) -> Schema {
        let mut keys = vec![];
        for k in v["keys"].as_array().cloned().unwrap_or_default() {
            let t = match k["t"].as_str().unwrap_or("") { "Bool" => KeyType::Bool, "Int" => KeyType::Int, "Enum" | "NativeEnum" => KeyType::Enum, "Float" => KeyType::Float, "Vector" => KeyType::Vector, "Rotator" => KeyType::Rotator, "Object" | "Class" => KeyType::Object, _ => KeyType::Other };
            keys.push((k["n"].as_str().unwrap_or("").to_string(), t, k["e"].as_str().map(String::from)));
        }
        let index = keys.iter().enumerate().map(|(i, k)| (k.0.clone(), i as u16)).collect();
        Schema { keys, index }
    }
    pub fn key(&self, name: &str) -> Option<u16> { self.index.get(name).copied() }
    pub fn default_val(t: KeyType) -> Val { match t { KeyType::Bool => Val::Bool(false), KeyType::Int | KeyType::Enum => Val::Int(0), KeyType::Float => Val::Float(0.), KeyType::Vector | KeyType::Rotator => Val::Vec(None), _ => Val::Obj(None) } }
}

pub struct Blackboard { pub schema: Arc<Schema>, pub vals: Vec<Val> }

impl Blackboard {
    pub fn new(schema: Arc<Schema>) -> Blackboard { let vals = schema.keys.iter().map(|k| Schema::default_val(k.1)).collect(); Blackboard { schema, vals } }
    pub fn k(&self, name: &str) -> Option<u16> { self.schema.key(name) }
    pub fn get(&self, k: u16) -> Val { self.vals[k as usize] }
    pub fn set(&mut self, k: u16, v: Val) { self.vals[k as usize] = v; }
    pub fn int(&self, name: &str) -> i32 { match self.k(name).map(|k| self.get(k)) { Some(Val::Int(i)) => i, Some(Val::Bool(b)) => b as i32, _ => 0 } }
    pub fn boolean(&self, name: &str) -> bool { match self.k(name).map(|k| self.get(k)) { Some(Val::Bool(b)) => b, Some(Val::Int(i)) => i != 0, Some(Val::Obj(o)) => o.is_some(), Some(Val::Vec(v)) => v.is_some(), _ => false } }
    pub fn float(&self, name: &str) -> f32 { match self.k(name).map(|k| self.get(k)) { Some(Val::Float(f)) => f, Some(Val::Int(i)) => i as f32, _ => 0. } }
    pub fn vector(&self, name: &str) -> Option<Vec3> { match self.k(name).map(|k| self.get(k)) { Some(Val::Vec(v)) => v, _ => None } }
    pub fn object(&self, name: &str) -> Option<u64> { match self.k(name).map(|k| self.get(k)) { Some(Val::Obj(o)) => o, _ => None } }
    pub fn set_int(&mut self, name: &str, v: i32) { if let Some(k) = self.k(name) { self.set(k, match self.schema.keys[k as usize].1 { KeyType::Bool => Val::Bool(v != 0), _ => Val::Int(v) }); } }
    pub fn set_bool(&mut self, name: &str, v: bool) { if let Some(k) = self.k(name) { self.set(k, Val::Bool(v)); } }
    pub fn set_float(&mut self, name: &str, v: f32) { if let Some(k) = self.k(name) { self.set(k, Val::Float(v)); } }
    pub fn set_vec(&mut self, name: &str, v: Option<Vec3>) { if let Some(k) = self.k(name) { self.set(k, Val::Vec(v)); } }
    pub fn set_obj(&mut self, name: &str, v: Option<u64>) { if let Some(k) = self.k(name) { self.set(k, Val::Obj(v)); } }
    pub fn is_set(&self, k: u16) -> bool { match self.get(k) { Val::Bool(b) => b, Val::Int(i) => i != 0, Val::Float(f) => f != 0., Val::Vec(v) => v.is_some(), Val::Obj(o) => o.is_some() } }
    pub fn number(&self, k: u16) -> f64 { match self.get(k) { Val::Bool(b) => b as i32 as f64, Val::Int(i) => i as f64, Val::Float(f) => f as f64, _ => 0. } }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OpKind { Basic, Arithmetic, Text }

pub fn test_basic(set: bool, op: i32) -> bool { if op == 0 { set } else { !set } }

pub fn test_arith(value: f64, op: i32, rhs: f64) -> bool {
    match op { 0 => value == rhs, 1 => value != rhs, 2 => value < rhs, 3 => value <= rhs, 4 => value > rhs, 5 => value >= rhs, _ => false }
}
