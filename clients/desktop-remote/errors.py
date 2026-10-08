# SPDX-License-Identifier: Apache-2.0
class BackendError(Exception):
    """A bounded public diagnostic, never a driver log or filesystem path."""
    def __init__(self, state):
        self.state = state
        super().__init__(state)
