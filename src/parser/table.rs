use combine::*;
use combine::char::char;
use combine::range::range;
use combine::primitives::RangeStream;
use parser::TomlParser;
use parser::errors::CustomError;
use parser::trivia::{line_trailing, ws};
use parser::key::key;
use key::Key;
use array_of_tables::ArrayOfTables;
use decor::{Decor, InternalString};
use table::{Item, Table};
use std::mem;
use std::cell::RefCell;
// https://github.com/rust-lang/rust/issues/41358
#[allow(unused_imports)]
use std::ops::DerefMut;


// table-key-sep   = ws %x2E ws  ; . Period
const TABLE_KEY_SEP: char = '.';
// std-table-open  = %x5B ws     ; [ Left square bracket
const STD_TABLE_OPEN: char = '[';
// std-table-close = ws %x5D     ; ] Right square bracket
const STD_TABLE_CLOSE: char = ']';
// array-table-open  = %x5B.5B ws  ; [[ Double left square bracket
const ARRAY_TABLE_OPEN: &str = "[[";
// array-table-close = ws %x5D.5D  ; ]] Double right quare bracket
const ARRAY_TABLE_CLOSE: &str = "]]";


// note: this rule is not present in the original grammar
// key-path = key *( table-key-sep key)
parse!(key_path() -> Vec<Key>, {
    sep_by1(between(ws(), ws(), key().map(|(raw, key)| Key::new(raw, key))),
            char(TABLE_KEY_SEP))
});


// ;; Standard Table

// std-table = std-table-open key *( table-key-sep key) std-table-close
parser!{
    fn std_table['a, 'b, I](parser: &'b RefCell<TomlParser>)(I) -> ()
        where
        [I: RangeStream<Range = &'a str, Item = char>,]
    {
        (
            between(char(STD_TABLE_OPEN), char(STD_TABLE_CLOSE),
                    key_path()),
            line_trailing(),
        )
            .and_then(|(h, t)|
                      parser
                      .borrow_mut()
                      .deref_mut()
                      .on_std_header(&h, t))
    }
}

// ;; Array Table

// array-table = array-table-open key *( table-key-sep key) array-table-close
parser!{
    fn array_table['a, 'b, I](parser: &'b RefCell<TomlParser>)(I) -> ()
        where
        [I: RangeStream<Range = &'a str, Item = char>,]
    {
        (
            between(range(ARRAY_TABLE_OPEN), range(ARRAY_TABLE_CLOSE),
                    key_path()),
            line_trailing(),
        )
            .and_then(|(h, t)|
                      parser
                      .borrow_mut()
                      .deref_mut()
                      .on_array_header(&h, t))
    }
}


// ;; Table

// table = std-table / array-table
parser!{
    pub fn table['a, 'b, I](parser: &'b RefCell<TomlParser>)(I) -> ()
        where
        [I: RangeStream<Range = &'a str, Item = char>,]
    {
        array_table(parser)
            .or(std_table(parser))
            .message("While parsing a Table Header")
    }
}

pub(crate) fn duplicate_key(path: &[Key], i: usize) -> CustomError {
    assert!(i < path.len());
    let header: Vec<&str> = path[..i].iter().map(|key| key.raw()).collect();
    CustomError::DuplicateKey {
        key: path[i].raw().into(),
        table: format!("[{}]", header.join(".")),
    }
}

impl TomlParser {
    fn descend_path<'a>(
        table: &'a mut Table,
        path: &[Key],
        i: usize,
    ) -> Result<&'a mut Table, CustomError> {
        if let Some(key) = path.get(i) {
            let mut new_table = Table::new();
            new_table.set_implicit(true);

            let entry = table.entry(key.raw());
            if entry.is_none() {
                *entry = Item::Table(new_table);
            }
            match *entry {
                Item::Value(..) => Err(duplicate_key(path, i)),
                Item::ArrayOfTables(ref mut array) => {
                    debug_assert!(!array.is_empty());

                    let index = array.len() - 1;
                    let last_child = array.get_mut(index).unwrap();

                    Self::descend_path(last_child, path, i + 1)
                }
                Item::Table(ref mut sweet_child_of_mine) => {
                    TomlParser::descend_path(sweet_child_of_mine, path, i + 1)
                }
                _ => unreachable!(),
            }
        } else {
            Ok(table)
        }
    }

    fn on_std_header(&mut self, path: &[Key], trailing: &str) -> Result<(), CustomError> {
        debug_assert!(!path.is_empty());

        let leading = mem::replace(&mut self.document.trailing, InternalString::new());
        let table = self.document.as_table_mut();

        let table = Self::descend_path(table, &path[..path.len() - 1], 0);
        let key = &path[path.len() - 1];

        match table {
            Ok(table) => {
                let decor = Decor::new(leading, trailing.into());

                let entry = table.entry(key.raw());
                if entry.is_none() {
                    *entry = Item::Table(Table::with_decor(decor));
                    self.current_table = entry.as_table_mut().unwrap();
                    return Ok(());
                }
                match *entry {
                    // if [a.b.c] header preceded [a.b]
                    Item::Table(ref mut t) if t.implicit => {
                        debug_assert!(t.values_len() == 0);
                        t.decor = decor;
                        t.set_implicit(false);
                        self.current_table = t;
                        return Ok(());
                    }
                    _ => {}
                }
                Err(duplicate_key(&path[..], path.len() - 1))
            }
            Err(e) => Err(e),
        }
    }

    fn on_array_header(&mut self, path: &[Key], trailing: &str) -> Result<(), CustomError> {
        debug_assert!(!path.is_empty());

        let leading = mem::replace(&mut self.document.trailing, InternalString::new());
        let table = self.document.as_table_mut();

        let key = &path[path.len() - 1];
        let table = Self::descend_path(table, &path[..path.len() - 1], 0);

        match table {
            Ok(table) => if !table.contains_table(key.get()) && !table.contains_value(key.get()) {
                let decor = Decor::new(leading, trailing.into());

                let entry = table
                    .entry(key.raw())
                    .or_insert(Item::ArrayOfTables(ArrayOfTables::new()));
                let array = entry.as_array_of_tables_mut().unwrap();
                self.current_table = array.append(Table::with_decor(decor));

                Ok(())
            } else {
                Err(duplicate_key(&path[..], path.len() - 1))
            },
            Err(e) => Err(e),
        }
    }
}
