# shop/extras has no __init__.py: a PEP 420 namespace subpackage.
from shop.core.base import Model


class AuditedModel(Model):
    def save(self):
        self.validate()
