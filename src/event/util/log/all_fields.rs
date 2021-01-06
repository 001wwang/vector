use crate::event::Value;
use serde::{Serialize, Serializer};
use std::{
    collections::{btree_map, BTreeMap},
    iter, slice,
};

/// Iterates over all paths in form "a.b[0].c[1]" in alphabetical order
/// and their corresponding values.
#[deprecated(since = "0.12.0", note = "Please use Event/Value pairs instead.")]
pub fn all_fields(
    fields: &BTreeMap<String, Value>,
) -> impl Iterator<Item = (String, &Value)> + Serialize {
    FieldsIter::new(fields)
}

#[derive(Clone)]
#[deprecated(since = "0.12.0", note = "Please use Event/Value pairs instead.")]
enum LeafIter<'a> {
    Map(btree_map::Iter<'a, String, Value>),
    Array(iter::Enumerate<slice::Iter<'a, Value>>),
}

#[derive(Clone)]
#[deprecated(since = "0.12.0", note = "Please use Event/Value pairs instead.")]
enum PathComponent<'a> {
    Key(&'a String),
    Index(usize),
}

/// Performs depth-first traversal of the nested structure.
#[derive(Clone)]
#[deprecated(since = "0.12.0", note = "Please use Event/Value pairs instead.")]
struct FieldsIter<'a> {
    /// Stack of iterators used for the depth-first traversal.
    stack: Vec<LeafIter<'a>>,
    /// Path components from the root up to the top of the stack.
    path: Vec<PathComponent<'a>>,
}

impl<'a> FieldsIter<'a> {
    fn new(fields: &'a BTreeMap<String, Value>) -> FieldsIter<'a> {
        FieldsIter {
            stack: vec![LeafIter::Map(fields.iter())],
            path: vec![],
        }
    }

    fn push(&mut self, value: &'a Value, component: PathComponent<'a>) -> Option<&'a Value> {
        match value {
            Value::Map(map) => {
                self.stack.push(LeafIter::Map(map.iter()));
                self.path.push(component);
                None
            }
            Value::Array(array) => {
                self.stack.push(LeafIter::Array(array.iter().enumerate()));
                self.path.push(component);
                None
            }
            _ => Some(value),
        }
    }

    fn pop(&mut self) {
        self.stack.pop();
        self.path.pop();
    }

    fn make_path(&mut self, component: PathComponent<'a>) -> String {
        let mut res = String::new();
        let mut path_iter = self.path.iter().chain(iter::once(&component)).peekable();
        loop {
            match path_iter.next() {
                None => return res,
                Some(PathComponent::Key(key)) => res.push_str(&key),
                Some(PathComponent::Index(index)) => res.push_str(&format!("[{}]", index)),
            }
            if let Some(PathComponent::Key(_)) = path_iter.peek() {
                res.push('.');
            }
        }
    }
}

impl<'a> Iterator for FieldsIter<'a> {
    type Item = (String, &'a Value);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.stack.last_mut() {
                None => return None,
                Some(LeafIter::Map(map_iter)) => match map_iter.next() {
                    None => self.pop(),
                    Some((key, value)) => {
                        if let Some(scalar_value) = self.push(value, PathComponent::Key(key)) {
                            return Some((self.make_path(PathComponent::Key(key)), scalar_value));
                        }
                    }
                },
                Some(LeafIter::Array(array_iter)) => match array_iter.next() {
                    None => self.pop(),
                    Some((index, value)) => {
                        if let Some(scalar_value) = self.push(value, PathComponent::Index(index)) {
                            return Some((
                                self.make_path(PathComponent::Index(index)),
                                scalar_value,
                            ));
                        }
                    }
                },
            };
        }
    }
}

impl<'a> Serialize for FieldsIter<'a> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_map(self.clone())
    }
}

#[cfg(test)]
mod test {
    use super::super::test::fields_from_json;
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_simple() {
        let fields = fields_from_json(json!({
            "field2": 3,
            "field1": 4,
            "field3": 5
        }));
        let expected: Vec<_> = vec![
            ("field1", &Value::Integer(4)),
            ("field2", &Value::Integer(3)),
            ("field3", &Value::Integer(5)),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v))
        .collect();

        let collected: Vec<_> = all_fields(&fields).collect();
        assert_eq!(collected, expected);
    }

    #[test]
    fn keys_nested() {
        let fields = fields_from_json(json!({
            "a": {
                "b": {
                    "c": 5
                },
                "a": 4,
                "array": [null, 3, {
                    "x": 1
                }, [2]]
            }
        }));
        let expected: Vec<_> = vec![
            ("a.a", &Value::Integer(4)),
            ("a.array[0]", &Value::Null),
            ("a.array[1]", &Value::Integer(3)),
            ("a.array[2].x", &Value::Integer(1)),
            ("a.array[3][0]", &Value::Integer(2)),
            ("a.b.c", &Value::Integer(5)),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v))
        .collect();

        let collected: Vec<_> = all_fields(&fields).collect();
        assert_eq!(collected, expected);
    }
}
