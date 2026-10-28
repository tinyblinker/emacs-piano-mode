;;; -*- lexical-binding: t -*-

(defun piano-play-key ()
  "get input envent and send to 'piano-key-down-function' and 'piano-key-up-function'"
  (interactive)
  (if t (piano-key-down) (piano-key-up)))

(defun piano-key-down ()
  (interactive)
  (message "key-down"))

(defun piano-key-up ()
  (interactive)
  (message "key-up"))
;;; Piano-mode.el ends here
