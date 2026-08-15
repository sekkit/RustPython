from typing import Generic, TypeVar
T = TypeVar('T')
class A(Generic[T]):
    pass
print('typing ok')
