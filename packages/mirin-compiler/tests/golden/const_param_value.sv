module reverse #(parameter int n) (
    input  logic clk,
    input  logic [7:0] v [0:n-1],
    output logic [7:0] result [0:n-1]
);
    logic [7:0] rev [0:n-1];
    for (genvar i = 0; i < n; i++) begin : g_i
        assign rev[i] = v[((n - 1) - i)];
    end
    assign result = rev;
endmodule
