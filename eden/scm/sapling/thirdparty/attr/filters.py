"""
Commonly useful filters for :func:`attr.asdict`.
"""



from ._compat import isclass
from ._make import Attribute


def _split_what(what):
    """
    Returns a tuple of `frozenset`s of classes and attributes.
    """
    return (
        frozenset(cls for cls in what if isclass(cls)),
        frozenset(cls for cls in what if isinstance(cls, Attribute)),
    )


def include(*what):
    r"""
    Include *what*.

    :param what: What to include.
    :type what: :class:`list` of :class:`type` or :class:`attr.Attribute`\ s

    :rtype: :class:`callable`
    """
    cls, attrs = _split_what(what)

    def include_(attribute, value):
        return value.__class__ in cls or attribute in attrs

    return include_


def exclude(*what):
    r"""
    Exclude *what*.

    :param what: What to exclude.
    :type what: :class:`list` of classes or :class:`attr.Attribute`\ s.

    :rtype: :class:`callable`
    """
    cls, attrs = _split_what(what)

    def exclude_(attribute, value):
        return value.__class__ not in cls and attribute not in attrs

    return exclude_
