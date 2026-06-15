package auth;

/** Validates an incoming auth token. */
public interface IValidator {
    boolean validate(String token);
}
