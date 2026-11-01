import logging

log = logging.getLogger(__name__)


class Model:
    def save(self):
        log.info("saving")
        self._write()

    def validate(self):
        return True

    def pong(self):
        return "pong"

    def _write(self):
        pass
