// The display names the backend's enum values map to.
//
// Their own module because three views need them: the environment list renders
// a status, the detail pane renders both, and the refresh diff names a role.

export const STATUS_LABEL = {
  stopped: 'Stopped',
  starting: 'Starting…',
  running: 'Running',
  failed: 'Failed',
};

export const ROLE_LABEL = {
  primary: 'Primary',
  replica: 'Read Replica',
};
