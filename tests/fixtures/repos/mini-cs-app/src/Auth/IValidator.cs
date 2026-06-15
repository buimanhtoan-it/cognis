namespace MiniApp.Auth
{
    /// <summary>Validates an incoming auth token.</summary>
    public interface IValidator
    {
        bool Validate(string token);
    }
}
