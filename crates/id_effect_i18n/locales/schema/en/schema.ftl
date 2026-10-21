### Validation messages of id_effect `Schema` decoding (id_effect_i18n feature `schema`).
### One message per `SchemaErrorCode`. Every message receives `$field` (the last segment of the
### issue's path) and `$path` (the whole path); the others are listed per message.

schema-missing-field = This field is required.
schema-required = A value is required.
schema-expected-object = Expected an object.
schema-expected-integer = Expected a whole number.
schema-expected-number = Expected a number.
schema-expected-string = Expected text.
schema-expected-boolean = Expected true or false.
schema-expected-array = Expected a list.
# $length: the number of items the list must have.
schema-expected-array-length = { $length ->
    [one] Expected a list of { $length } item.
   *[other] Expected a list of { $length } items.
}
# $value: the only accepted value.
schema-expected-literal = Expected { $value }.
schema-not-empty = Must not be empty.
# $min: the fewest characters.
schema-min-length = { $min ->
    [one] Must be at least { $min } character long.
   *[other] Must be at least { $min } characters long.
}
# $max: the most characters.
schema-max-length = { $max ->
    [one] Must be at most { $max } character long.
   *[other] Must be at most { $max } characters long.
}
# $pattern: the regular expression.
schema-pattern = Must match the pattern { $pattern }.
schema-email = Must be a valid email address.
schema-uuid = Must be a valid UUID.
schema-url = Must be a valid URL.
# $values: the accepted values, comma-separated.
schema-one-of = Must be one of: { $values }.
# $min: the smallest accepted value.
schema-minimum = Must be at least { $min }.
# $max: the largest accepted value.
schema-maximum = Must be at most { $max }.
# $min: the value the input must exceed.
schema-exclusive-minimum = Must be greater than { $min }.
# $max: the value the input must stay below.
schema-exclusive-maximum = Must be less than { $max }.
# $min: the fewest items.
schema-min-items = { $min ->
    [one] Must have at least { $min } item.
   *[other] Must have at least { $min } items.
}
# $max: the most items.
schema-max-items = { $max ->
    [one] Must have at most { $max } item.
   *[other] Must have at most { $max } items.
}
schema-no-match = Does not match any of the accepted forms.
