;;; -*- lexical-binding: t -*-

;; (core) keys mapped here to be played
(defun piano-play-key ()
  "get input envent and send to 'piano-key-down-function' and 'piano-key-up-function'"
  (interactive)
  (let* ((key last-input-event))
    (if (piano-key-is-down key) (piano-key-down key)
      (piano-key-up key))))

;; if the piano's 'key' is down
(defun piano-key-is-down (key))

;; the pianos's 'key' is down
(defun piano-key-down (key)
  (interactive)
  (message "key-down:%s" key))

;; the piano's 'key' is up
(defun piano-key-up (key)
  (interactive)
  (message "key-up:%s" key))
;;; Piano-mode.el ends here
