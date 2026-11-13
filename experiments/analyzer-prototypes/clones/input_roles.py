"""Bounded TypeScript input regrouping for whole-body clone review leads."""
from dataclasses import dataclass
from pathlib import Path
from typing import assert_never

from bindings import Bindings, Unsupported, tokens


def compare_input_roles(old, other):
    try:
        left, right = describe(old), describe(other)
        if left.signature != right.signature:
            raise Unsupported('different_callable_or_return_shape')
        if len(left.inputs) != len(right.inputs):
            raise Unsupported('different_flattened_input_count')
        if len(left.body) != len(right.body):
            raise Unsupported('different_complete_executable_body')
        mapping, reverse = {}, {}
        for a, b in zip(left.body, right.body):
            if a.startswith('input:') and b.startswith('input:'):
                if mapping.setdefault(a, b) != b or reverse.setdefault(b, a) != a:
                    raise Unsupported('inconsistent_input_uses')
            elif a != b:
                raise Unsupported('different_complete_executable_body')
        if len(mapping) != len(left.inputs):
            raise Unsupported('unused_inputs')
        evidence, crossings, named_anchor = [], 0, False
        for a, b in sorted(mapping.items()):
            first, second = left.inputs[int(a[6:])], right.inputs[int(b[6:])]
            if isinstance(first, PropertyInput) and isinstance(second, PropertyInput):
                if first.key != second.key:
                    raise Unsupported('different_object_property_role')
            elif isinstance(first, PositionalInput) and isinstance(second, PositionalInput):
                if first.position != second.position or first.annotation != second.annotation:
                    raise Unsupported('different_positional_role_or_type')
            else:
                crossings += 1
                positional = first if isinstance(first, PositionalInput) else second
                prop = first if isinstance(first, PropertyInput) else second
                named_anchor |= positional.name == prop.key
            evidence.append(dict(left=role_evidence(first), right=role_evidence(second)))
        if not crossings:
            raise Unsupported('no_parameter_regrouping')
        if not named_anchor:
            raise Unsupported('no_named_regrouping_anchor')
        return dict(decision='accepted', reason='same_complete_body_after_input_regrouping',
                    input_mapping=evidence, regrouped_inputs=crossings,
                    return_aliases=dict(left=left.return_alias, right=right.return_alias))
    except Unsupported as error:
        return dict(decision='rejected', reason=str(error))


def describe(unit):
    if Path(unit.path).suffix not in {'.ts', '.tsx'}:
        raise Unsupported('input_regrouping_is_typescript_only')
    function = unit.node
    if function.type != 'function_declaration':
        raise Unsupported('input_regrouping_requires_named_function')
    pending = [function]
    while pending:
        node = pending.pop()
        if 'identifier' in node.type and '\\' in text(node, unit.source):
            raise Unsupported('escaped_identifier')
        pending.extend(node.named_children)
    if function.child_by_field_name('type_parameters') is not None:
        raise Unsupported('generic_callable')
    parameters = function.child_by_field_name('parameters')
    if parameters is None or parameters.type != 'formal_parameters':
        raise Unsupported('unsupported_parameter_list')
    bindings = InputBindings(unit.source)
    bindings.flatten(parameters)
    bindings.visit(unit.body)
    annotation = function.child_by_field_name('return_type')
    return_names, alias = return_alias_names(annotation, unit.body, unit.source)
    names = bindings.names | return_names
    signature = (function.type,) + tokens(function, unit.source, names, excluded=[
        unit.body, parameters, function.child_by_field_name('name'),
    ])
    if function.parent is not None and function.parent.type == 'export_statement':
        signature += tokens(function.parent, unit.source, {}, excluded=[function])
    return Description(signature, tokens(unit.body, unit.source, names), bindings.inputs, alias)


def return_alias_names(annotation, body, source):
    if annotation is None:
        return {}, None
    children = meaningful_children(annotation)
    if len(children) != 1:
        raise Unsupported('unsupported_return_annotation')
    declared = children[0]
    if declared.type == 'generic_type':
        name = declared.child_by_field_name('name')
        arguments = declared.child_by_field_name('type_arguments')
        if name is None or text(name, source) != 'Promise' or arguments is None:
            return {}, None
        arguments = meaningful_children(arguments)
        if len(arguments) != 1:
            return {}, None
        declared = arguments[0]
    if declared.type != 'type_identifier':
        return {}, None
    statements = meaningful_children(body)
    if not statements or statements[-1].type != 'return_statement':
        return {}, None
    returned = meaningful_children(statements[-1])
    if len(returned) != 1:
        return {}, None
    expression = returned[0]
    while expression.type in {'await_expression', 'parenthesized_expression'}:
        children = meaningful_children(expression)
        if len(children) != 1:
            return {}, None
        expression = children[0]
    if expression.type != 'as_expression':
        return {}, None
    parts = meaningful_children(expression)
    if len(parts) != 2 or parts[1].type != 'type_identifier':
        return {}, None
    asserted = parts[1]
    if text(asserted, source) != text(declared, source):
        return {}, None
    replacement = ('return_alias',)
    return {declared.start_byte: replacement, asserted.start_byte: replacement}, text(declared, source)


def role_evidence(role):
    if isinstance(role, PositionalInput):
        return dict(kind='positional', position=role.position, name=role.name,
                    annotation=list(role.annotation))
    if isinstance(role, PropertyInput):
        return dict(kind='property', position=role.position, name=role.name, key=role.key)
    assert_never(role)


@dataclass(frozen=True)
class PositionalInput:
    position: int
    name: str
    annotation: tuple


@dataclass(frozen=True)
class PropertyInput:
    position: int
    name: str
    key: str


@dataclass(frozen=True)
class Description:
    signature: tuple
    body: tuple
    inputs: list[PositionalInput | PropertyInput]
    return_alias: str | None


class InputBindings(Bindings):
    def __init__(self, source):
        super().__init__(source)
        self.inputs = []

    def flatten(self, parameters):
        keys = set()
        for position, parameter in enumerate(meaningful_children(parameters)):
            if parameter.type != 'required_parameter' or parameter.child_by_field_name('value') is not None:
                raise Unsupported('optional_default_or_unsupported_parameter')
            pattern = parameter.child_by_field_name('pattern')
            annotation = parameter.child_by_field_name('type')
            allowed = {node.id for node in (pattern, annotation) if node is not None}
            if any(child.id not in allowed for child in meaningful_children(parameter)):
                raise Unsupported('parameter_modifiers')
            if pattern is None:
                raise Unsupported('missing_parameter_pattern')
            if pattern.type == 'identifier':
                role = PositionalInput(position, self.text(pattern),
                    tokens(annotation, self.source, {}) if annotation else ())
                self.bind_input(pattern, role)
            elif pattern.type == 'object_pattern':
                types = meaningful_children(annotation) if annotation is not None else []
                if len(types) != 1 or types[0].type != 'type_identifier':
                    raise Unsupported('object_parameter_requires_simple_named_type')
                fields = meaningful_children(pattern)
                if not fields:
                    raise Unsupported('empty_object_parameter')
                for field in fields:
                    if field.type == 'shorthand_property_identifier_pattern':
                        key, binding = field, field
                    elif field.type == 'pair_pattern':
                        key = field.child_by_field_name('key')
                        binding = field.child_by_field_name('value')
                        if key is None or key.type != 'property_identifier' or binding is None or binding.type != 'identifier':
                            raise Unsupported('nested_computed_or_default_property')
                    else:
                        raise Unsupported('unsupported_object_parameter_field')
                    key_name = self.text(key)
                    if key_name in keys:
                        raise Unsupported('duplicate_object_property')
                    keys.add(key_name)
                    self.bind_input(binding, PropertyInput(position, self.text(binding), key_name))
            else:
                raise Unsupported('unsupported_parameter_pattern')

    def bind_input(self, node, role):
        name = self.text(node)
        if name in self.scopes[0]:
            raise Unsupported('duplicate_input_binding')
        slot = f'input:{len(self.inputs)}'
        self.inputs.append(role)
        self.scopes[0][name] = slot
        self.names[node.start_byte] = (slot,)

    def visit(self, node):
        if (node is not None and node.type in {'identifier', 'shorthand_property_identifier'}
                and self.text(node) == 'arguments'):
            raise Unsupported('arguments_depends_on_parameter_packing')
        super().visit(node)


def meaningful_children(node):
    return [child for child in node.named_children if 'comment' not in child.type]


def text(node, source):
    return source[node.start_byte:node.end_byte].decode(errors='replace')
