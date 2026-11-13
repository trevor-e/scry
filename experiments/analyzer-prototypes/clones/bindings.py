"""Conservative local binding normalization; never a semantic equivalence proof."""
from dataclasses import dataclass


def fingerprint(function, body, source):
    """Use declaration slots, or preserve all names when binding syntax is unsupported."""
    try:
        bindings = Bindings(source)
        bindings.parameters(function)
        bindings.visit(body)
        names = bindings.names
        mode, reason = 'resolved_locals', None
    except Unsupported as error:
        names = {}
        mode, reason = 'literal_identifiers', str(error)
    function_name = function.child_by_field_name('name')
    signature = tokens(function, source, names, excluded=[body, function_name])
    return Fingerprint(mode, signature, tokens(body, source, names), reason)


@dataclass(frozen=True)
class Fingerprint:
    mode: str
    signature: tuple
    body: tuple
    fallback_reason: str | None


class Unsupported(Exception):
    pass


class Bindings:
    def __init__(self, source):
        self.source = source
        self.scopes = [{}]
        self.names = {}
        self.next_slot = 0

    def parameters(self, function):
        parameters = function.child_by_field_name('parameters')
        if parameters is None:
            parameter = function.child_by_field_name('parameter')
            if parameter is not None:
                self.bind(parameter)
            return
        defaults = []
        for parameter in parameters.named_children:
            if 'comment' in parameter.type:
                continue
            if parameter.type == 'self_parameter':
                continue
            if parameter.type in {'required_parameter', 'optional_parameter', 'parameter'}:
                pattern = parameter.child_by_field_name('pattern')
                if pattern is None or pattern.type != 'identifier':
                    raise Unsupported('destructured or non-identifier parameter')
                self.bind(pattern)
                defaults.append(parameter.child_by_field_name('value'))
            elif parameter.type == 'identifier':
                self.bind(parameter)
            else:
                raise Unsupported('unsupported parameter syntax: ' + parameter.type)
        # JavaScript parameter defaults see all parameters, including their TDZ.
        for default in defaults:
            self.visit(default)

    def visit(self, node):
        if node is None or 'comment' in node.type:
            return
        kind = node.type
        if kind.startswith('jsx_'):
            raise Unsupported('JSX identifier roles')
        if kind == 'identifier' and self.text(node) == 'eval':
            raise Unsupported('dynamic eval scope')
        if kind in {'type_annotation', 'type_arguments', 'type_parameters', 'scoped_identifier', 'scoped_type_identifier'}:
            return
        if kind in {'macro_invocation', 'let_condition', 'match_expression', 'catch_clause',
                    'variable_declaration', 'function_declaration', 'function_item', 'function_expression',
                    'class_declaration', 'class', 'with_statement', 'labeled_statement',
                    'method_definition', 'switch_statement', 'using_declaration',
                    'const_item', 'static_item', 'type_item', 'use_declaration',
                    'type_alias_declaration', 'interface_declaration'}:
            raise Unsupported('unsupported binding context: ' + kind)
        if kind in {'block', 'statement_block'}:
            self.scopes.append({})
            # TS bindings include the temporal dead zone; later declarations still shadow.
            if kind == 'statement_block':
                for statement in node.named_children:
                    if statement.type == 'lexical_declaration':
                        self.predeclare(statement)
            for child in node.named_children:
                self.visit(child)
            self.scopes.pop()
            return
        if kind == 'lexical_declaration':
            for declaration in node.named_children:
                if declaration.type == 'variable_declarator':
                    name = declaration.child_by_field_name('name')
                    if name.type != 'identifier':
                        raise Unsupported('destructured lexical declaration')
                    if name.start_byte not in self.names:
                        self.bind(name)
                    self.visit(declaration.child_by_field_name('value'))
            return
        if kind == 'let_declaration':
            pattern = node.child_by_field_name('pattern')
            if pattern is None or pattern.type != 'identifier':
                raise Unsupported('destructured Rust let')
            # Rust's initializer sees the prior binding when let shadows a name.
            self.visit(node.child_by_field_name('value'))
            self.bind(pattern)
            return
        if kind == 'for_expression':
            self.visit(node.child_by_field_name('value'))
            self.scopes.append({})
            self.bind(node.child_by_field_name('pattern'))
            self.visit(node.child_by_field_name('body'))
            self.scopes.pop()
            return
        if kind == 'for_in_statement':
            left = node.child_by_field_name('left')
            keyword = node.child_by_field_name('kind')
            if keyword is None or self.text(keyword) not in {'let', 'const'}:
                raise Unsupported('non-lexical TypeScript loop binding')
            self.scopes.append({})
            self.bind(left)
            # The lexical loop binding shadows outer names even in its RHS (TDZ).
            self.visit(node.child_by_field_name('right'))
            self.visit(node.child_by_field_name('body'))
            self.scopes.pop()
            return
        if kind == 'for_statement':
            raise Unsupported('TypeScript loop binding scope')
        if kind in {'arrow_function', 'closure_expression'}:
            self.scopes.append({})
            self.parameters(node)
            self.visit(node.child_by_field_name('body'))
            self.scopes.pop()
            return
        if kind in {'identifier', 'shorthand_property_identifier', 'shorthand_field_identifier'}:
            for scope in reversed(self.scopes):
                if self.text(node) in scope:
                    slot = scope[self.text(node)]
                    if kind.startswith('shorthand') or node.parent.type == 'shorthand_field_initializer':
                        self.names[node.start_byte] = ('key:' + self.text(node), slot)
                    else:
                        self.names[node.start_byte] = (slot,)
                    break
            return
        for child in node.named_children:
            self.visit(child)

    def predeclare(self, declaration):
        for child in declaration.named_children:
            if child.type == 'variable_declarator':
                self.bind(child.child_by_field_name('name'))

    def bind(self, node):
        if node is None or node.type != 'identifier':
            raise Unsupported('non-identifier binding')
        slot = f'local:{self.next_slot}'
        self.next_slot += 1
        self.scopes[-1][self.text(node)] = slot
        self.names[node.start_byte] = (slot,)

    def text(self, node):
        return self.source[node.start_byte:node.end_byte].decode(errors='replace')


def tokens(root, source, names, excluded=()):
    result = []
    excluded_ids = {node.id for node in excluded if node is not None}

    def visit(node):
        if node.id in excluded_ids or 'comment' in node.type:
            return
        if (node.type == ',' and node.parent.type in {'formal_parameters', 'parameters', 'type_parameters', 'type_arguments'}
                and node.next_sibling is not None and node.next_sibling.type in {')', '>'}):
            return
        if not node.children:
            result.extend(names.get(node.start_byte, (node.type + ':' + source[node.start_byte:node.end_byte].decode(errors='replace'),)))
        else:
            for child in node.children:
                visit(child)

    visit(root)
    return tuple(result)
