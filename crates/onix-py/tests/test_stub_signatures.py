"""Checks that ``deepdiff_rs.pyi`` cannot drift from the built module.

For every function, method, and property the stub declares, this compares
its parameter names, defaults, and keyword-only markers against
``inspect.signature()`` of the real, compiled object — so a signature change
on either side that is not mirrored on the other fails a test, rather than
surfacing later as a wrong IDE tooltip or a `mypy` false negative.
"""

import ast
import inspect
from pathlib import Path

import pytest

import deepdiff_rs

STUB_PATH = Path(__file__).resolve().parent.parent / "deepdiff_rs.pyi"


def _params(args: ast.arguments) -> list[tuple[str, bool, bool]]:
    """One ``(name, has_default, keyword_only)`` tuple per stub parameter,
    in declaration order, skipping ``self``."""
    result = []
    positional = args.posonlyargs + args.args
    defaults_start = len(positional) - len(args.defaults)
    for i, arg in enumerate(positional):
        if arg.arg == "self":
            continue
        result.append((arg.arg, i >= defaults_start, False))
    for arg, default in zip(args.kwonlyargs, args.kw_defaults):
        result.append((arg.arg, default is not None, True))
    return result


def _runtime_params(func: object) -> list[tuple[str, bool, bool]]:
    """The same shape as :func:`_params`, from a real callable's
    ``inspect.signature``, skipping ``self``."""
    result = []
    for name, param in inspect.signature(func).parameters.items():
        if name == "self":
            continue
        result.append(
            (
                name,
                param.default is not inspect.Parameter.empty,
                param.kind is inspect.Parameter.KEYWORD_ONLY,
            )
        )
    return result


def _stub_module() -> ast.Module:
    return ast.parse(STUB_PATH.read_text())


def _stub_functions(module: ast.Module) -> dict[str, ast.FunctionDef]:
    return {node.name: node for node in module.body if isinstance(node, ast.FunctionDef)}


def _stub_classes(module: ast.Module) -> dict[str, ast.ClassDef]:
    return {node.name: node for node in module.body if isinstance(node, ast.ClassDef)}


def _is_property(node: ast.FunctionDef) -> bool:
    return any(isinstance(d, ast.Name) and d.id == "property" for d in node.decorator_list)


def _class_members(node: ast.ClassDef) -> dict[str, ast.FunctionDef]:
    return {n.name: n for n in node.body if isinstance(n, ast.FunctionDef)}


MODULE = _stub_module()
FUNCTIONS = _stub_functions(MODULE)
CLASSES = _stub_classes(MODULE)


def test_stub_declares_the_whole_public_surface() -> None:
    """Every public name the built module actually exports has a stub entry, and vice versa.

    Compares against `dir(deepdiff_rs)` rather than a literal set: a public
    symbol added to the module with no matching stub entry fails this test
    instead of passing silently.
    """
    module_names = {n for n in dir(deepdiff_rs) if not n.startswith("_") and n != "deepdiff_rs"}
    stub_names = set(FUNCTIONS) | set(CLASSES) | {"MAX_DEPTH_CEILING"}
    assert module_names == stub_names
    assert isinstance(deepdiff_rs.MAX_DEPTH_CEILING, int)


@pytest.mark.parametrize("name", sorted(FUNCTIONS))
def test_module_function_signature_matches_the_stub(name: str) -> None:
    stub_params = _params(FUNCTIONS[name].args)
    runtime_params = _runtime_params(getattr(deepdiff_rs, name))
    assert stub_params == runtime_params


def test_deepdiff_init_signature_matches_the_stub() -> None:
    init = _class_members(CLASSES["DeepDiff"])["__init__"]
    stub_params = _params(init.args)
    runtime_params = _runtime_params(deepdiff_rs.DeepDiff)
    assert stub_params == runtime_params


def _every_stub_method() -> list[tuple[str, str]]:
    """Every non-``__init__``, non-property method declared on a stub class —
    generated from the stub itself, so a method added to the stub without a
    matching entry here cannot go unchecked."""
    pairs = []
    for class_name, node in CLASSES.items():
        for method_name, member in _class_members(node).items():
            if method_name == "__init__" or _is_property(member):
                continue
            pairs.append((class_name, method_name))
    return sorted(pairs)


@pytest.mark.parametrize(("class_name", "method_name"), _every_stub_method())
def test_method_signature_matches_the_stub(class_name: str, method_name: str) -> None:
    node = _class_members(CLASSES[class_name])[method_name]
    stub_params = _params(node.args)
    real_class = getattr(deepdiff_rs, class_name)
    runtime_params = _runtime_params(getattr(real_class, method_name))
    assert stub_params == runtime_params


@pytest.mark.parametrize(("class_name", "property_name"), [("TableDiff", "schema"), ("TableDiff", "schema_arrow")])
def test_property_is_declared_as_a_property_in_both_places(class_name: str, property_name: str) -> None:
    node = _class_members(CLASSES[class_name])[property_name]
    assert _is_property(node), f"{class_name}.{property_name} must be declared with @property in the stub"
    real_class = getattr(deepdiff_rs, class_name)
    # A PyO3 `#[getter]` is a `getset_descriptor`, not a Python `property`,
    # but both are non-callable data descriptors accessed without `()` —
    # unlike a method (a function or method-descriptor).
    descriptor = inspect.getattr_static(real_class, property_name)
    assert not inspect.isfunction(descriptor)
    assert not inspect.ismethoddescriptor(descriptor)
    assert hasattr(descriptor, "__get__")


def test_max_depth_error_is_a_value_error_subclass_in_both_places() -> None:
    (base,) = CLASSES["MaxDepthError"].bases
    assert isinstance(base, ast.Name) and base.id == "ValueError"
    assert issubclass(deepdiff_rs.MaxDepthError, ValueError)
