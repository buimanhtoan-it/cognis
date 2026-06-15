namespace MiniApp
{
    using MiniApp.Auth;

    public class Program
    {
        public static void Main()
        {
            var service = new AuthService(new JwtValidator());
            service.Login("token");
        }
    }
}
